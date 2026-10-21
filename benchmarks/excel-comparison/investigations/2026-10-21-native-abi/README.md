# Native input ABI local qualification — 2026-10-21

Implemented automatic B/A/J/D% scalar input selection, strict/presence Q selection, and shared ABI planning. Returns, panic boundaries, lifecycle and async handle protocols retain their existing paths.

Timing scope: macOS ARM64 production input admission, without Excel, COM, generated entrypoint dispatch, handle lookup/publication, or recalculation. Q and native paths live in the same frozen executable, with identical fixtures and call setup. This is an input-ABI comparison, not an old/new revision comparison.

Protocol: Q/native/native/Q then native/Q/Q/native; four process runs per ABI, 30 Criterion samples per case per run, 1 second measurement and 150 ms warmup. Table values are medians of four per-run mean estimates in ns per call. Negative change means native is faster. Both per-block changes are retained in comparison.json; small changes remain timing noise candidates. Source and executable hashes are in provenance.json.

| Input / mode | Q ns | Native ns | Change | ABBA / BAAB change |
| --- | ---: | ---: | ---: | ---: |
| bool/materialize_identity | 232.2 | 238.8 | +2.9% | +2.9% / +3.2% |
| bool/plain | 24.7 | 24.4 | -1.3% | -0.9% / -2.3% |
| bool/prepare_identity | 232.4 | 239.0 | +2.8% | +2.8% / +2.6% |
| f64_1/materialize_identity | 233.4 | 241.0 | +3.3% | +1.0% / +3.7% |
| f64_1/plain | 24.1 | 24.6 | +1.9% | +1.7% / +1.8% |
| f64_1/prepare_identity | 233.9 | 242.4 | +3.7% | -2.2% / +3.8% |
| f64_2/materialize_identity | 384.9 | 381.7 | -0.8% | -1.6% / +0.3% |
| f64_2/plain | 24.6 | 25.0 | +1.7% | +1.8% / +1.7% |
| f64_2/prepare_identity | 385.9 | 382.3 | -0.9% | -1.6% / +0.3% |
| f64_4/materialize_identity | 602.2 | 601.7 | -0.1% | -0.4% / +0.5% |
| f64_4/plain | 28.0 | 26.4 | -5.8% | -6.0% / -5.8% |
| f64_4/prepare_identity | 600.8 | 599.6 | -0.2% | -0.7% / +0.4% |
| f64_8/materialize_identity | 1034.1 | 1026.8 | -0.7% | -0.4% / -0.7% |
| f64_8/plain | 37.0 | 28.3 | -23.5% | -24.1% / -23.1% |
| f64_8/prepare_identity | 1028.7 | 1028.3 | -0.0% | -0.3% / -0.2% |
| i32/materialize_identity | 235.2 | 237.4 | +1.0% | +0.9% / +1.7% |
| i32/plain | 24.7 | 24.3 | -1.5% | -1.2% / -4.6% |
| i32/prepare_identity | 233.7 | 237.1 | +1.4% | +1.4% / +0.3% |
| unicode_long/materialize_identity | 24409.0 | 24425.7 | +0.1% | -0.9% / +0.4% |
| unicode_long/plain | 4094.9 | 4087.9 | -0.2% | +0.0% / -0.6% |
| unicode_long/prepare_identity | 20484.0 | 20375.6 | -0.5% | -0.2% / -0.2% |
| unicode_short/materialize_identity | 384.5 | 383.8 | -0.2% | -0.6% / -0.2% |
| unicode_short/plain | 73.5 | 58.7 | -20.2% | -20.8% / -19.4% |
| unicode_short/prepare_identity | 339.1 | 341.3 | +0.7% | -0.0% / +1.4% |

Observed tradeoffs: ordinary conversion improved for eight doubles (-23.5%) and short Unicode (-20.2%). One-double ordinary conversion regressed +1.9%; Boolean input identity regressed +2.8 to +2.9% in the median estimates, with the same direction in both ordering blocks. Long Unicode paths were roughly unchanged (-0.5% to +0.1%). These are local path measurements, not universal or live Excel speed claims.

`plain` converts and materializes; `prepare_identity` validates and fingerprints and discards the prepared string without allocating its owned value; `materialize_identity` also creates the String. Scalar materialization is trivial. The Unicode inputs are `日本語💡` and that text repeated 800 times (4,000 UTF-16 units). Fixture construction is outside timing; ordinary call admission and scratch cleanup remain inside.

Validation: workspace nextest 1,310 passed / 11 skipped; strict workspace all-target/all-feature Clippy, strict rustdoc, guide examples (17 chapters), 5 native ingress tests under strict-provenance Stacked and Tree Borrows Miri, and x86_64/i686 MSVC library cross-checks passed. Excel comparison Python tests: 80 passed / 1 skipped. All 288 feature-powerset checks passed. The standalone SDK probe is extended, but native Windows + Microsoft SDK execution is pending.

Live Excel qualification remains pending: input coercion/rounding, blank/missing/error behavior and performance improvements/regressions are not established by this local bundle. N01–N05 provide strict/native recalculation pairs; the separate N06 instrumented build records admitted-layer entry and Rust rejection. Zero observed layer entry leaves Excel preprocessing versus Rust pre-admission rejection unresolved. Malformed UTF-16 is covered by direct input tests rather than fabricated Excel cells.

Reproduce local cases with `rtk just bench-native-abi`. See ../../README.md for the Windows build and run commands. The full bench-ci/bench-scaling suites were not run for this input-ABI qualification.
