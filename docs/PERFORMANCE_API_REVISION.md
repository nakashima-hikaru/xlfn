# Prepared input and recoverable RTD send API

Date: 2026-09-29. Base: `246bb97`.

## Public contracts

`PrepareExcel<'call>` replaces the boolean `FromExcel::PREFLIGHT` contract.
Its associated `Prepared` type retains validated intermediate state or the
converted value. Materialization consumes that state on a formula miss; a hit
drops it. Custom implementations explicitly provide both operations, with no
decode/discard fallback. Ordinary UDF arguments still require only `FromExcel`.
`ExcelEnum` derives preparation alongside conversion and semantic identity.

`Option`, `OptionalExcelValue`, `Matrix`, `Vec`, `Row`, `Column`, and
`BoundedVarArgs` compose preparation. Custom element states are retained exactly
once; built-in numeric/String sequences validate and hash borrowed cells without
allocating their output on a hit. The internal sequence helper enforces cell and
allocation budgets. Plain-input containers retain their original single-pass
conversion. Context-bearing views and handle pins remain eager, defaults are
retained, and async ingress finishes conversion before scheduling.

`RtdSender::try_send` returns `RtdSendError::{Invalid, Full, Closed}`. Full and
Closed carry an opaque `RtdPendingValue<T>`; `try_send_pending` retries without
cloning or repeating user conversion. Conversion occurs before admission, even
for an already closed sender. Invalid conversion has no recoverable payload.
`into_error` explicitly discards a pending update for producer-error propagation;
there is no implicit lossy `From` conversion. Accepted updates still use the
same bounded FIFO and generation publisher pool; this revision does not change
delivery or shutdown semantics.

Usage and contracts: [custom conversions](reference/custom-conversions.md),
[RTD guide](../guide/src/rtd.md).

## Final API boundary review

The collection hook is no longer a supported public extension point. Public
`PrepareExcel` implementations provide `Prepared`, `prepare`, and `materialize`.
The `ExcelInputCells` and `PreparedExcelSequence` facade exports were removed.
Stable Rust lacks specialization for the blanket argument implementation, so
built-in dispatch retains a hidden `__prepare_elements` method using types from
an inaccessible module. This is an explicitly excluded internal hook, not a
promise that the public trait has no other method in its source definition.
It preserves built-in allocation behavior without a public preflight flag or
unsafe type discrimination.

`RtdPendingValue<T>` is invariant in the input type, and `RtdSendError<T>` carries
it through Full/Closed. Retrying on a sender with another input type is rejected.
A same-type sender from another subscription remains valid. The consumed input
is not stored, so the marker does not require T to implement Clone, Debug, Send,
or Sync. Debug implementations avoid accidentally adding derived generic bounds.
Invalid errors retain their standard error source.

External compile contracts now cover a boxed custom prepared type, a borrowed
prepared type reused through its associated-type projection, all seven supported
container forms, and retry with an input that has no Clone/Debug/Send support.
Compile-fail contracts reject borrowed state escaping to static, access to the
internal collection types, and cross-input-type RTD retry. The supported contract
and exclusions are recorded in [API stability](API_STABILITY.md).

This follow-up changes type boundaries and dispatch visibility, not the
collection algorithm or publisher topology. The measurements below predate
this follow-up; no new timing claim is made for it.

## Measurement

Same macOS arm64 optimized benchmark configuration as the
[production adoption](PERFORMANCE_REDESIGN_PRODUCTION.md). Two fresh processes,
each ABBA order; no concurrent builds/tests during timing. Values are means of
four path means in microseconds/call, not statistical significance estimates.
The eager control and prepared candidate both use the current source.

| Input | Path | Eager us | Prepared us | Change |
|---|---|---:|---:|---:|
| f64 | warm | 0.355 | 0.347 | -2.3% |
| f64 | cold | 1.042 | 1.046 | +0.3% |
| f64 | changed_last | 1.038 | 1.044 | +0.6% |
| matrix_f64_1k | warm | 14.466 | 14.137 | -2.3% |
| matrix_f64_1k | cold | 15.236 | 17.102 | +12.2% |
| matrix_f64_1k | changed_last | 15.214 | 17.073 | +12.2% |
| matrix_f64_100k | warm | 1142.678 | 1102.600 | -3.5% |
| matrix_f64_100k | cold | 1154.543 | 1394.063 | +20.7% |
| matrix_f64_100k | changed_last | 1150.281 | 1332.309 | +15.8% |
| matrix_string_10k | warm | 727.716 | 557.048 | -23.5% |
| matrix_string_10k | cold | 733.956 | 888.252 | +21.0% |
| matrix_string_10k | changed_last | 729.949 | 889.899 | +21.9% |

Warm numeric 100k retains the allocation reduction 3 -> 2, requested bytes
806,106 -> 6,106. Warm String 10k retains 10,003 -> 2 allocations and
376,106 -> 6,106 bytes. Cold built-in sequences still traverse twice; this API
revision does not remove that tradeoff. Custom prepared state avoids repeating
custom conversion, which is covered by regression tests rather than this timing
fixture. These measurements exclude generated ABI and Excel/COM execution.

Raw data: [A](measurements/2026-09-29-redesign/api-two-phase-a.jsonl),
[B](measurements/2026-09-29-redesign/api-two-phase-b.jsonl).

## Verification

- Workspace all-feature nextest: 979 passed, 11 skipped.
- Serialized xlfn all-feature libtests: 711 passed, 10 ignored.
- Generated signature compile-pass/fail suite passes after the final macro
  inference adjustment; snapshots retain actionable missing-trait diagnostics.
- Workspace all-target/all-feature clippy, formatting, panic boundary policy,
  strict rustdoc, core-only check, and standalone RTD example check pass.
- i686/x86_64 Windows MSVC all-feature checks with `blake3/pure` pass.
- Strict-provenance Miri passes under both Stacked Borrows and Tree Borrows:
  two preparation tests, two production ingress fixtures, and one non-Clone
  RTD retry test. Coverage includes retained custom state, nested container
  identity equivalence, warm materialization skipping, invalid final cells,
  and Full/Closed recovery without repeated conversion.

For the final API boundary follow-up, nextest (979 passed, 11 skipped), the
expanded external compile suite, serialized libtests, Clippy, strict rustdoc,
core-only and RTD example checks, and both Windows cross-checks were rerun.
Miri results above belong to the preceding implementation; this type/visibility
follow-up adds no unsafe operations and was not separately rerun under Miri.

No live Windows/Excel execution or new formal refinement claim is included.
