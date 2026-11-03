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
| `argument_ingress`, `formula_revision` | Scalar and matrix conversion, input identity, prepared inputs, and formula reuse |
| `array_numeric_output`, `array_string_output`, `value_boundary_allocations` | Numeric/string returns and conversion allocation traffic |
| `registration`, `formula_caller`, `sync_boundary` | Registration preparation, caller resolution, and UDF boundary overhead |
| `handle_prepare`, `handle_lookup`, `handle_call_resolution` | Handle publication, lookup, and multi-argument resolution |
| `object_lease` | Shared-object pins and final pin release |
| `cache_registry`, `cache_miss_concurrency`, `cache_reclamation` | Endpoint lookup, concurrent misses, eviction, and lease reclamation |
| `async_spawn`, `async_task_drain`, `async_task_drain_allocations`, `async_pending_allocations` | Task spawning, rescheduling, cancellation, and drain allocation budgets |
| `rtd_prepare`, `rtd_prepare_allocations`, `rtd_connection_allocations`, `rtd_publish`, `rtd_refresh` | Subscription admission, owner storage, publication, and refresh |

Run one maintained target with its required features:

```sh
just bench-one argument_ingress
just bench-one cache_registry "bench-internals cache"
just bench-one rtd_publish "bench-internals rtd"
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

Measurements execute Rust paths without Excel/COM. Requested bytes, allocation
traffic, and process RSS describe different quantities. For performance
comparisons, use the same workloads, features, compiler, and machine, with
separate target directories for the compared builds.
