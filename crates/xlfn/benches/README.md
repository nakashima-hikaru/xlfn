# Benchmark notes

## `cache_lookup`

This benchmark compares the production `CalculationCache<u64, u64>` with a
benchmark-only control that keeps the same versioned Moka cache, capacity, key
shape, warm hit rate, and persistent worker topology, but stores `(Arc<u64>,
weight)` instead of the production node pointer and lease protocol.

The lookup cases are:

- one warm key with one worker;
- one hot key with 1, 2, 8, and 32 workers;
- one disjoint key per worker with 1, 2, 8, and 32 workers;
- deterministic live-lease retirement with a one-entry cache.

The steady-state hit rows also include two benchmark-only diagnostic controls:

- `no_admission_control`: the same Moka lookup and node pin, without lookup admission;
- `no_pin_control`: the same Moka lookup and lookup admission, with raw node access and no pin accounting.

Together with `current` and `arc_control`, these controls separate admission and
pin costs without adding either mechanism to the production cache API. The
diagnostic controls assume that the warmed cache is not evicted or mutated
while the worker pool is running.

Run the full benchmark with the normal ten-second measurement policy:

```text
cargo bench -p xlfn --bench cache_lookup --features bench-internals,unstable-cache
```

For a short local smoke run, set `XLFN_BENCH_MEASUREMENT_MS` and reduce the
warm-up time:

```text
XLFN_BENCH_MEASUREMENT_MS=50 cargo bench -p xlfn --bench cache_lookup \
  --features bench-internals,unstable-cache -- --noplot --warm-up-time 0.05
```

The allocation probe runs outside Criterion's timed section and reports
allocator calls from one warm batch after the worker pool and cache have
already been initialized.

## 2026-09-05 smoke result

The lookup rows below used 50 ms measurement time, 100 samples, and 50 ms
warm-up. The live-lease row used the same measurement time with Criterion's
default warm-up. Values are the time for one benchmark batch; each batch
contains 1,000 hits per worker. These numbers validate the benchmark and are
not a replacement for a full ten-second comparison on the target deployment
host.

| Case | Current | Arc control |
| --- | ---: | ---: |
| warm, 1 worker | 114.97 µs | 107.50 µs |
| hot key, 1 worker | 114.87 µs | 109.08 µs |
| hot key, 2 workers | 276.75 µs | 279.50 µs |
| hot key, 8 workers | 1.1808 ms | 1.6017 ms |
| hot key, 32 workers | 4.7483 ms | 8.5208 ms |
| disjoint, 1 worker | 115.57 µs | 109.10 µs |
| disjoint, 2 workers | 228.12 µs | 209.56 µs |
| disjoint, 8 workers | 960.78 µs | 955.29 µs |
| disjoint, 32 workers | 3.8993 ms | 3.8682 ms |
| live-lease retirement, 1 worker | 699.19 µs | 588.79 µs |

The warm-hit allocation probe reported 8 allocations for current and 0 for
the Arc control in this run. The current-path allocation count is diagnostic
and should be investigated separately from the ownership comparison; it is
not folded into the Criterion timing conclusion.

## 2026-09-05 current vs Arc full run

The requested ten-second run completed before adding the diagnostic controls.
Values are the median time for one batch of 1,000 hits per worker:

| Case | Current | Arc control |
| --- | ---: | ---: |
| warm, 1 worker | 114.89 µs | 108.35 µs |
| hot key, 1 worker | 114.38 µs | 108.42 µs |
| hot key, 2 workers | 263.88 µs | 262.21 µs |
| hot key, 8 workers | 1.4032 ms | 1.6133 ms |
| hot key, 32 workers | 5.9926 ms | 7.0730 ms |
| disjoint, 1 worker | 116.89 µs | 109.68 µs |
| disjoint, 2 workers | 216.68 µs | 220.02 µs |
| disjoint, 8 workers | 1.1044 ms | 1.1793 ms |
| disjoint, 32 workers | 4.9006 ms | 4.1825 ms |
| live-lease retirement, 1 worker | 691.61 µs | 576.87 µs |

## 2026-09-05 diagnostic run

The diagnostic rows used 1 s measurement time and 50 samples, with a 3 s
follow-up for the 32-worker rows. Values are batch medians:

| Case | Current | No admission | No pin | Arc control |
| --- | ---: | ---: | ---: | ---: |
| warm, 1 worker | 116.79 µs | 93.75 µs | 95.80 µs | 110.44 µs |
| hot key, 1 worker | 116.65 µs | 93.50 µs | 95.96 µs | 109.71 µs |
| hot key, 32 workers | 6.6656 ms | 6.5591 ms | 6.1722 ms | 7.3071 ms |
| disjoint, 1 worker | 116.97 µs | 94.42 µs | 96.15 µs | 110.16 µs |
| disjoint, 32 workers | 4.2866 ms | 4.0903 ms | 3.9854 ms | 4.0644 ms |

There is no `miss_singleflight` row in this benchmark yet.
