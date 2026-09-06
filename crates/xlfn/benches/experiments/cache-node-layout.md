# Cache node payload layout experiment

Decision: retain `Box<V>` in `CacheNode<V>` for now. Directly storing `V`
removes one allocation for nonzero-sized values, but the measured tradeoff
depends on value size and alignment. Large inline arrays consumed more
allocator space and took longer to construct and reclaim in this experiment.

This is a standalone layout and allocation microbenchmark, **not a measurement
of production cache performance**. It does not execute Moka, lookup admission,
pins, eviction, or grace-period maintenance, and it is not a Cargo bench target.
The executable mirrors the metadata fields of `CacheNode` while replacing its
domain pointer's pointee with `()`; the pointer is never dereferenced. Recheck
the metadata against `crates/xlfn/src/cache.rs` when revisiting this experiment.

## Reproduce

From the repository root, using Rust 1.98 or later:

```sh
rustc --edition 2024 -O \
  crates/xlfn/benches/experiments/cache_node_layout.rs \
  -o /tmp/xlfn-cache-node-layout
/tmp/xlfn-cache-node-layout
```

On Windows, select a writable output path ending in `.exe`. No external Rust
dependencies are required. Layout sizes, allocation counts, timings, and the
destructor-panic check work on supported Rust hosts. Actual allocator block
sizes use macOS `malloc_size`; other hosts print `unavailable` for those fields.
Use the default unwind panic strategy; `-C panic=abort` cannot run the panic test.

The timing probe alternates the variant order across nine rounds, excludes the
first round, and reports the median of eight per-round average durations. Each
iteration creates and destroys one node and constructs its payload. `max_ns`
is the largest **round average**, not per-operation p99. Allocation accounting
runs separately from timing; the allocator still checks its inactive accounting
flag during timing. `black_box` prevents the node allocations from disappearing.

## Recorded result

Recorded on 2026-09-07 with Apple M1, macOS 26.6.2 (25G83), and
`rustc 1.98.0 (88d9e12ae 2026-08-18)`, host `aarch64-apple-darwin`, LLVM 22.1.8,
using `-O` and the System allocator. These are single-host exploratory results,
without CPU isolation or a statistical production-performance gate.

| Value | Requested bytes, boxed | Requested bytes, direct | Allocator bytes, boxed | Allocator bytes, direct |
| --- | ---: | ---: | ---: | ---: |
| `u64` | 48 | 40 | 64 | 48 |
| `Vec<u8>` containing 8 KiB | 64 | 56 | 80 | 64 |
| `[u8; 8192]` | 8,232 | 8,224 | 8,240 | 10,240 |
| `[u8; 65536]` | 65,576 | 65,568 | 65,584 | 81,920 |
| 64-byte payload with alignment 64 | 104 | 128 | 112 | 128 |
| `()` | 40 | 32 | 48 | 32 |

Byte totals include the node allocation and the separate `Box<V>` allocation
when present. The `Vec` row excludes its unchanged 8 KiB backing allocation in
both variants. Requested bytes are Rust layout sizes; allocator bytes include
size-class rounding but are not process RSS.

| Value | Iterations per round | Boxed median ns/iteration | Direct median ns/iteration |
| --- | ---: | ---: | ---: |
| `u64` | 500,000 | 52.92 | 30.03 |
| `Vec<u8>` containing 8 KiB | 50,000 | 189.13 | 157.45 |
| `[u8; 8192]` | 50,000 | 267.66 | 355.68 |
| `[u8; 65536]` | 10,000 | 1,862.64 | 3,352.50 |

For 1,024 nodes, both the `u64` and 8 KiB array probes recorded 2,048
allocations/deallocations with boxed values and 1,024 with direct values.
The large-array allocator results show why fewer allocations need not consume
less memory: adding node metadata to a power-of-two payload crosses a size-class
boundary. Alignment can also increase the combined node's padding.

## Destruction and interpretation

The boxed variant moves its `Box<V>` into `catch_unwind`, matching the existing
reclamation shape. The direct variant catches `drop(Box<DirectNode<V>>)` so a
large `V` is not moved out into a stack-resident unwind closure. Both contained
an ordinary destructor panic and released all counted allocations in the probe:
4 allocated/4 freed for boxed, 3 allocated/3 freed for direct, including the
panic runtime's allocations. This check does not establish every panic-payload
or reentrant-destruction property of the full cache.

Direct storage helps the small-value construction cases here. It also changes
payload movement, allocation size classes, alignment, and proximity to the
frequently modified pin counter. Those differences require full-cache tests
before making a generic representation change. The current evidence supports
retaining separate payload storage instead of assuming allocation count alone
determines performance.

Reconsider direct storage only with Windows cache measurements covering small
and large values, hot and disjoint keys, eviction, and 1/8/32 threads. Include
per-operation tail latency, allocation counts, allocator footprint, and maximum
retirement debt, together with destructor-panic and reentrant-drop regressions.
This experiment provides a reproducible comparison and a reason to defer the
change; it does not claim the boxed representation is optimal for every workload.
