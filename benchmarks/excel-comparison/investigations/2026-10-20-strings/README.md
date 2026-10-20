# Excel-DNA comparison follow-up: strings

## Source and scope

User-supplied results: 256 records, 128 pairs; 121 comparable pairs, 45 with a primary metric behind Excel-DNA. Seven pairs include an error and are excluded. All comparisons refer to CI commit `1e569b71a4f52564914df454120a199f5d7e530c`, run `39176112243`, attempt 1. The original Windows artifact predates both the matrix improvements and this string change.

Local baseline is the working tree at the start of this follow-up, **including the previously applied matrix improvements**. Identical new benchmark fixtures were added to both source snapshots. The candidate changes only UTF length reductions and fixed-width BMP return encoding. No new Windows/Excel result is claimed.

## Disposition of other gaps

| Cases | Assessment and action |
| --- | --- |
| T01–T03 | Optimize UTF-16→UTF-8 length planning and UTF-8→UTF-16 length planning; encode proven three-byte BMP return strings directly. Fixed-size subtotals retain exact lengths and strict surrogate errors. Standard mixed/astral fallback remains. |
| M01–M05 small matrices | The previous numeric validation/builder changes remain applied. The reported COM-inclusive millisecond differences are not explained solely by nanosecond/microsecond local paths; no speculative allocation or ABI rewrite. |
| A02/10000us; A03/100; R01/100 | End-to-end Excel scheduling, cold submission and polling are plausible contributors. A03/100 native release-to-settle is 169ms vs RTD-task 22ms, with different delivery mechanisms; not an isolated executor measurement. No cancellation/ownership changes based on this single observation. |
| P01/8; P03/8,16 | Approximately 0–25% differences in busy-loop workloads; CPU scheduling/clock variation is an estimate, not a demonstrated bottleneck. No arithmetic or thread-safety semantics changed. |
| R04 | The observed rate is about 9 updates/s at requested 1,000–2,000 Hz under 100ms RTD throttling; most pair gaps are below 0.1%, the largest about 2.3%. No notification semantics changed to chase polling variation. |
| C01 | XLL load is 20.7ms vs 208.7ms even though startup-to-ready is 2.833s vs 1.182s. Stage timestamps place the main excess before RegisterXLL. Process/bootstrap timing is a stronger explanation than slow XLL registration. |
| C03/C04; S01/1000 | First-call/workbook-open and small scalar totals include Excel/COM overhead; no isolated framework defect established. |
| L01 scalar/matrix/RTD | Primary metric is incremental process RSS per cell, not latency. xlfn starts at ~186MB vs ~218–220MB and has lower absolute peak RSS in all four L01 workloads. These differences alone do not establish a leak or per-value allocation problem. |

## Applied changes and local results

1. UTF-16→UTF-8 planning sums at most 64 units in `u16` before widening to `usize` (maximum subtotal 192). UTF-8→UTF-16 planning sums at most 64 bytes in `u8` (maximum subtotal 128). This reduces the vector lane width without changing counts or validation.
2. Return storage directly decodes three-byte UTF-8 chunks when `UTF-8 bytes == 3 × UTF-16 units`. For valid Rust strings this proves all scalars are non-surrogate BMP characters. ASCII, mixed and astral text retain their existing semantics.
3. The non-ASCII length reduction is outlined so its register setup does not burden the ASCII path. An earlier candidate had an 8.3% short-ASCII owned-return regression; the final candidate resolves that case. Some other ASCII controls still regress slightly, listed below.

macOS arm64, Rust 1.99.0, optimized bench profile, `bench-internals`; separate full source snapshots and target directories, frozen SHA-256 executables. Final order is candidate/baseline/baseline/candidate (BAAB), 30 samples per case per run, 100ms warmup and 500ms measurement. Reported values are medians of the two run medians on each side. The preceding unoutlined candidate was additionally tested in eight ABBA/BAAB rounds; its results are not used as final-candidate results. No concurrent builds/tests ran during the paired timings.

All 63 final cases and per-run medians are in `paired-summary.json`; 252 Criterion estimate/sample records are in `criterion-observations.json`. Percentages below measure only the local Rust paths, not end-to-end Excel improvements. `scalar_output/*/owned` includes cloning a prebuilt UTF-8 value; the Excel T02 fixture constructs a repeated string instead. Copy probes include strict input decoding, output encoding, publication and cleanup; they exclude Excel/COM and formula submission.

| Local case | Baseline ns | Candidate ns | Change |
| --- | ---: | ---: | ---: |
| argument_ingress/string_ascii_4k_owned | 2018.9 | 542.6 | -73.12% |
| argument_ingress/string_japanese_4k_owned | 2209.8 | 739.0 | -66.56% |
| scalar_output/japanese_4k_owned | 9286.3 | 1310.1 | -85.89% |
| scalar_output/japanese_4k_copy | 11366.7 | 1824.0 | -83.95% |
| scalar_output/ascii_4k_copy | 2287.6 | 741.7 | -67.58% |
| scalar_output/unicode_1k_owned | 1016.8 | 776.3 | -23.65% |
| scalar_output/ascii_short_owned | 127.8 | 115.4 | -9.73% |
| scalar_output/ascii_1k_borrowed | 121.1 | 129.5 | +6.95% |
| scalar_output/ascii_4k_borrowed | 232.7 | 242.4 | +4.17% |
| callback_counted_utf16/ascii_1k | 87.9 | 90.8 | +3.31% |
| argument_ingress/f64_plain | 24.1 | 24.1 | +0.07% |
| scalar_output/number | 39.4 | 39.4 | -0.04% |

Remaining regressions include borrowed ASCII 1KiB (~8ns, 7%) and 4KiB (~10ns, 4%), and callback ASCII 1KiB (~3ns, 3%). These are disclosed rather than generalized away as noise. Two process runs per side do not establish universal performance; verify the workload mix on Windows.

## Validation

Passed: 1,296 workspace tests (11 skipped), all 288 feature configurations, strict Clippy/rustdoc, 3 targeted Miri tests in each of the default and Tree Borrows + strict provenance modes, the Windows x64 MSVC fixture cross-check, formatting/diff checks, panic-boundary audit, and all 20 canonical bench-ci targets.

Validation results and applied-source hashes are recorded in `validation.json` and `applied-source-hashes.json`. Focused tests cover 63/64/65 and 127/128/129 subtotal boundaries, strict surrogate errors across chunk boundaries, mixed/astral fallback, all three-byte BMP scalar values, and existing exact Excel length limits. Ownership, allocation limits, return ABI, numeric behavior and async/RTD scheduling are unchanged by this follow-up.

`bench-ci` is a short execution/contract gate, using a 100ms measurement override while other validation may run. Its timing numbers are not used for performance conclusions. Windows MSVC cross-check is compilation/type evidence only; new native Windows builds and live Excel comparisons remain unexecuted.

Re-run the existing Excel runner for T01/T02/T03 and the small matrix cases with new XLLs from the same source revision, preserve both XLL hashes, and reverse implementation order across independent Excel sessions. Original logs have only five recalculation observations, so small percent differences and tail quantiles need repetition before deployment claims.

The snapshots/frozen binaries and detailed execution logs remain in `/private/tmp/xlfn-strings-20261019-uy451w0u`. The repository bundle preserves executable/source hashes, paired timings, Criterion samples, normalized original observations, and all 128 original pair classifications. `paired.py` is the exact runner used in that temporary experiment and expects its frozen binaries at the paths recorded in `executables.json`.

## Original losing primary metrics

Factor >1 means the xlfn primary metric is worse. Throughput factors invert the raw quotient. RSS and latency metrics are intentionally kept distinct.

| Case | Metric | xlfn | Excel-DNA | Factor |
| --- | --- | ---: | ---: | ---: |
| A02/10000us | cell_latency_p50_s | 0.156784 | 0.152434 | 1.0285 |
| A03/100 | throughput_per_s | 102.751 | 136.286 | 1.3264 |
| C01/cold | startup_to_ready_s | 2.83289 | 1.18172 | 2.3973 |
| C03/first | first_call_s | 0.515777 | 0.345566 | 1.4926 |
| C04/warm-open | open_to_complete_s | 0.486779 | 0.39354 | 1.2369 |
| L01/matrix | peak_rss_per_cell_bytes | 3497.16 | 2003.35 | 1.7457 |
| L01/rtd | peak_rss_per_cell_bytes | 5136.38 | 4882.02 | 1.0521 |
| L01/scalar | peak_rss_per_cell_bytes | 3499.21 | 2519.04 | 1.3891 |
| M01/10x10 | ns_per_element | 21918 | 13564 | 1.6159 |
| M02/1000x100 | ns_per_element | 73.047 | 53.683 | 1.3607 |
| M02/100x100 | ns_per_element | 289.52 | 117.57 | 2.4625 |
| M02/10x10 | ns_per_element | 16399 | 7425 | 2.2086 |
| M03/100x100 | ns_per_element | 197.74 | 136.32 | 1.4506 |
| M03/10x10 | ns_per_element | 14051 | 7560 | 1.8586 |
| M04/1000x10 | ns_per_element | 264.07 | 139.22 | 1.8968 |
| M04/100x100 | ns_per_element | 251.81 | 164.62 | 1.5296 |
| M04/10x1000 | ns_per_element | 271.9 | 165.03 | 1.6476 |
| M05/10x10 | p50_s | 0.0044293 | 0.0019554 | 2.2652 |
| P01/8 | calls_per_s | 43911.8 | 46516.4 | 1.0593 |
| P03/1000us-16 | calls_per_s | 8740.4 | 8957.2 | 1.0248 |
| P03/1000us-8 | calls_per_s | 7600.34 | 7602.41 | 1.0003 |
| P03/100us-16 | calls_per_s | 58767.9 | 62594.3 | 1.0651 |
| P03/100us-8 | calls_per_s | 50417.2 | 62809.7 | 1.2458 |
| R01/100 | subscription_s | 0.64851 | 0.490335 | 1.3226 |
| R04/1000hz | observed_updates_per_s | 9.09487 | 9.09924 | 1.0005 |
| R04/10hz | observed_updates_per_s | 8.9958 | 8.99801 | 1.0002 |
| R04/1hz | observed_updates_per_s | 0.899621 | 0.899671 | 1.0001 |
| R04/2000hz | observed_updates_per_s | 8.89849 | 9.09894 | 1.0225 |
| S01/1000 | p50_s | 0.0077436 | 0.0075179 | 1.0300 |
| T01/ascii-4096 | ns_per_char | 6.2917 | 5.04617 | 1.2468 |
| T01/ascii-512 | ns_per_char | 29.7789 | 29.3553 | 1.0144 |
| T01/ja-4096 | ns_per_char | 11.3568 | 8.93467 | 1.2711 |
| T02/ascii-64 | ns_per_char | 211.833 | 167.378 | 1.2656 |
| T02/ascii-8 | ns_per_char | 2142.6 | 1465.61 | 1.4619 |
| T02/ja-4096 | ns_per_char | 19.121 | 11.0182 | 1.7354 |
| T02/ja-512 | ns_per_char | 67.0625 | 52.0227 | 1.2891 |
| T02/ja-64 | ns_per_char | 255.356 | 230.244 | 1.1091 |
| T03/ascii-4096 | ns_per_char | 17.2962 | 12.4489 | 1.3894 |
| T03/ascii-512 | ns_per_char | 70.8469 | 59.4586 | 1.1915 |
| T03/ascii-64 | ns_per_char | 252.222 | 189.227 | 1.3329 |
| T03/ascii-8 | ns_per_char | 1365.84 | 830.75 | 1.6441 |
| T03/ja-4096 | ns_per_char | 37.1437 | 25.3055 | 1.4678 |
| T03/ja-512 | ns_per_char | 121.562 | 89.1064 | 1.3642 |
| T03/ja-64 | ns_per_char | 720.245 | 520.491 | 1.3838 |
| T03/ja-8 | ns_per_char | 6197.96 | 3413.37 | 1.8158 |
