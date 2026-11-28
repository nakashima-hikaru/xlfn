# Benchmarks

Run commands from the repository root. [Justfile](../../../Justfile) defines
maintained regression and scaling suites; [Cargo.toml](../Cargo.toml) registers
the targets and required features.

```sh
just bench-ci       # Representative regression suite; also used by bench and bench-pr.
just bench-scaling  # Worker, batch, and input-size curves.
just bench-full     # Unfiltered runs of the maintained targets.
just bench-check    # Clippy for every benchmark with all features.
```

The benchmark workflow runs `bench-ci` on pushes, pull requests, and its nightly
schedule. Bencher stores performance history when configured. Generated timings,
allocation reports, and Criterion data remain build artifacts under `target/`;
the repository does not keep results from individual machines.

| Targets | Coverage |
| --- | --- |
| `argument_ingress` | Scalar and matrix conversion, native vs `Q` ABI admission, input identity, and prepared inputs |
| `array_numeric_output`, `array_string_output`, `value_boundary_allocations` | Numeric/string returns and conversion/registration allocation budgets |
| `sync_boundary` | Synchronous UDF admission, return, and error boundary overhead |
| `handle_prepare`, `handle_lookup` | Handle publication and token lookup |
| `cache_registry`, `cache_miss_concurrency` | Endpoint lookup, borrowed keys, and concurrent single-flight misses |
| `async_spawn`, `async_task_drain_allocations`, `async_pending_allocations` | Task spawning, rescheduling, cancellation, and drain allocation budgets |
| `rtd_prepare_allocations`, `rtd_connection_allocations`, `rtd_publish`, `rtd_publish_allocations`, `rtd_refresh` | Subscription allocation budgets, publication, and refresh |

Run one maintained target with its required features:

```sh
just bench-one argument_ingress
just bench-one cache_registry "bench-internals cache"
just bench-one rtd_publish "bench-internals rtd"
just bench-one rtd_publish_allocations "bench-internals rtd"
just bench-one async_spawn "bench-internals async-builtin"
```

`just bench-native-abi` selects strict `Q` and native `B`, `A`, `J`, and `D%`
admission cases within `argument_ingress`. Representative pairs also run in
`bench-ci`. Real worksheet recalculation is covered by the separate
[Excel comparison harness](../../../benchmarks/excel-comparison/README.md).

Criterion targets normally measure for ten seconds per case.
`XLFN_BENCH_MEASUREMENT_MS` provides a local override; Criterion warmup and sample
count options are separate. Allocation targets use deterministic iteration
counts and check their stated budgets independently of timing.

The `rtd_publish_string_mix` Criterion group and `rtd_publish_allocations` probe
compare moved, cloned, and borrowed short and 8 KiB text at 0%, 50%, 99%, and
100% duplicates. Moved-string allocation is deliberately outside the measurement
because the caller already owns the string; clone and borrowed-copy costs are
included. The allocation probe warms the pending slot and asserts its budgets.

Measurements execute Rust paths without Excel/COM. Requested bytes, allocation
traffic, and process RSS describe different quantities. For performance
comparisons, use the same workloads, features, compiler, and machine, with
separate target directories for the compared builds.
