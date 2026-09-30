# Async calculation-event qualification

Status on 2026-09-30: **live Excel unverified**. No local macOS test, Windows
cross-compilation, callback mock, or performance benchmark establishes native
event registration or removal. The current evidence record is
[records.json](records.json); its observation list is deliberately empty.

## Public contract and remaining uncertainty

Microsoft's [xlEventRegister documentation](https://learn.microsoft.com/en-us/office/client-developer/excel/xleventregister)
specifies a string procedure name, an integer event selector, and an integer
result greater than zero for success. Zero reports failure. xlfn accepts only
positive integer acknowledgements. Negative values and other result types do
not establish success.

The current removal operation passes `xltypeNil` as the procedure and the
event selector as `xltypeInt`. That removal convention is **not specified by
the public xlEventRegister page**. A positive acknowledgement alone therefore
does not establish that Excel stopped calling the handler. The convention
needs observed registration/removal behavior on each deployed configuration.
No undocumented negative-result compatibility rule is assumed.

Microsoft's [Handling Events documentation](https://learn.microsoft.com/en-us/office/client-developer/excel/handling-events)
describes CalculationCanceled followed by CalculationEnded for user
interruption, and states that these events are not raised during programmatic
recalculation. A COM-only calculation run cannot establish the interactive
event sequence. Cancellation remains best effort.

If removal has a malformed or nonpositive acknowledgement after a successful
callback transport, xlfn treats the host mutation as indeterminate. The
registration remains pending in the cleanup ledger, so it cannot serve as a
completed removal certificate. This preserves the unload hold rather than
claiming an unobserved host state.

## Qualification gate

Record one result for every exact Excel version, build, update channel, and
Excel process bitness distributed to. A record for one version or architecture
does not qualify another. Include both 32-bit and 64-bit Excel only when both
are distribution targets; each needs its matching native XLL. Identify Excel
bitness from Excel's About dialog and the running EXCEL.EXE architecture,
rather than the operating-system or Python bitness.

Use a fresh interactive Windows Excel process and a trusted instrumented XLL
from the candidate commit. Retain the XLL's SHA-256 and the instrumented
source/build diff. The existing comparison runner's version/build and XLL
metadata can accompany the record, but its worksheet results do not capture
raw event acknowledgements or prove nil-procedure removal.

1. Capture the actual `xlEventRegister` arguments, callback transport status,
   raw result `xltype`, signed integer value, and raw integer bits for both
   exported handlers: `__xlfn_calculation_canceled` and
   `__xlfn_calculation_ended`. Require positive integer acknowledgements.
2. Observe handler entry with invocation/generation identifiers. Exercise
   normal interactive calculation, user cancellation while async calls are
   pending, and completion after cancellation. Record ordering, outstanding
   tasks, and whether late results reached worksheet cells. Use programmatic
   recalculation as a separately recorded control.
3. Drain framework tasks while keeping the probe XLL loaded. Issue removal
   for each event using the actual nil-procedure convention. Capture the same
   raw arguments, transport status, result type/value/bits, and cleanup result.
   A transport success or positive integer alone is insufficient.
4. With the probe XLL still loaded and its counters available, repeat the same
   interactive calculation/cancellation sequences. Record counters before
   removal and after each sequence, and verify that the removed handlers
   receive no further calls. Keep logs for the complete observation interval.
5. Exercise repeated registration/removal and add-in close/reopen cycles.
   Verify exactly one callback per expected event after re-registration, no
   callbacks to a removed generation, and no retained event cleanup debt before
   approving unload. Preserve error and failure logs as well as successes.

Keep the probe XLL loaded, or end the whole test Excel process, when removal
cannot be established. Do not unload code merely to test whether Excel might
still retain its handler address. Any required case without evidence keeps
that configuration **unverified**; a contradictory observation marks it
**failed**. Record the exact observed value before considering a documented
compatibility decision.

## Record contents

Append real runs to `observations` in [records.json](records.json), preserving
earlier runs. Every observation must contain:

- UTC timestamp, operator, Windows version, Excel product/version/build/channel,
  explicit Excel process bitness, and process executable identity;
- candidate commit, XLL target architecture and SHA-256, instrumentation diff,
  calculation mode, multithreaded calculation settings, and test inputs;
- both registration and removal calls with procedure/event arguments, transport
  status, raw result type, signed integer and raw bits, and result cleanup;
- normal/canceled/programmatic event traces, post-removal counter observations,
  repeated-cycle traces, task/ledger state, and late-result checks;
- evidence-file paths and SHA-256 values, tested scope, failed or omitted cases,
  and a final `verified`, `failed`, or `unverified` verdict.

Use `verified` only after all required observations for that exact
configuration pass. Leave unavailable values absent or null and keep the
verdict `unverified`; do not turn a planned configuration into an observed run.
The following test coverage is local mock evidence only: positive integer
acceptance, nonpositive/wrong-type rejection, indeterminate event mutations,
and retention of an event after unconfirmed removal.
