# Public API review: array return ABI

Ordinary `Matrix<f64>`, `Row<f64>`, and `Column<f64>` now default to Q,
including fallible returns. `NumericArrayOutput` or `return_abi = "fp12"`
explicitly selects FP12 and its null / `#NUM!` failure contract. This makes
ordinary scalar and array `Result` error meanings agree. Existing worksheets
that rely on K% must explicitly opt into FP12 and re-register the rebuilt XLL.

The existing production return-path benchmark was run on macOS during API
implementation. These are exploratory means in nanoseconds, with concurrent
compilation and broad intervals in several cases. This is not an Excel or
Windows performance qualification, nor a stable performance threshold.

| Case | Cells | FP12 ns | Q ns | Q / FP12 |
|---|---:|---:|---:|---:|
| matrix_build_return | 100 | 207.3 | 177.4 | 0.86 |
| numeric_slice_convert_return | 100 | 174.2 | 133.8 | 0.77 |
| matrix_build_return | 100,000 | 65,684.1 | 98,943.7 | 1.51 |
| numeric_slice_convert_return | 100,000 | 41,525.3 | 64,227.6 | 1.55 |

Q is materially more expensive for the measured large arrays; small-array
results are noisy and do not establish an FP12 advantage. Keep the explicit
fast path for workloads that accept its error restriction. Error semantics
are the default policy; the measurements do not justify silently changing
application-selected errors to `#NUM!`.

`array-abi-benchmark.log` retains raw output. `criterion/` retains samples and
confidence intervals. `provenance.json` records the base commit, command,
compiler, host, executable hash and post-measurement source hashes. The patch
captures added tests in the return module; production return code and the
benchmark helper were unchanged. Cargo exit status was zero. The helper's
10-second measurement setting overrides the requested CLI 2 seconds.

See [VALIDATION.md](VALIDATION.md) for source/API/ownership checks.

## Migration and qualification

- Add `return_abi = "fp12"` where the old numeric-container ABI is required.
- `input_semantics = strict` makes ordinary input conversion independent of
  scalar alias spelling; explicit numeric views/references keep their ABI.
- Cache weight callbacks move to cache construction or static endpoints.
- Minimal `#[excel_addin]` implementations omit standard associated defaults.
- `Addin::open` failure remains terminal/quarantined. Recoverable acquisition
  needs a separate cleanup transaction contract and remains future work (P3).
