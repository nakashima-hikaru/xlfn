# Async calculation-event qualification

Status on 2026-10-25: **full event lifecycle qualification unverified**. No local macOS test, Windows
cross-compilation, callback mock, or performance benchmark establishes native
event registration or removal. The current evidence record is
[records.json](records.json); it now includes two partial live Windows shutdown
observations. Neither qualifies event removal.

## Live L02 observations, 2026-10-25

[Selected evidence](l02-2026-10-25.json) retains provenance, metric summaries,
consecutive-block timings, memory segments, slowest iterations and lifecycle
events from four supplied JSONL records. It includes the original input's
SHA-256; it is not a copy of all iteration samples. Both runner and XLLs used
`cd9fb4b95fc67af4dad8fd6ba98abd418c2b2986`, CI run `40175450611`, attempt 1.
Excel reported version `16.0`, build `20527.0`, and 64-bit Windows. Each run
used 1,000 cells, one calculation thread, manual calculation, and allocation
diagnostics. The update channel and executable identity were not captured.

Both xlfn runs recorded the same direct trigger for
`Quarantine(UnregisterFailed)`: removal of CalculationEnded (selector 1) and
CalculationCanceled (selector 2) returned callback transport `Success`, raw
`xltypeInt` (`0x0800`) with integer value `0`, and result cleanup `Ok`.
The decoder rejected zero, so removal stayed indeterminate. There were no
function-removal failure records or diagnostic-cap markers. This identifies
the failed acknowledgement check, **not whether Excel actually removed the
handlers**. The close callback reached `Quarantined` and returned status 1.
All four Excel processes exited naturally with exit code 0 and no forced kill.

| Implementation / repetitions | Rust allocated / freed bytes | RSS growth (MiB) | Private bytes growth (MiB) | p50 / p99 (ms) |
| --- | ---: | ---: | ---: | ---: |
| xlfn / 100 | 415,200 / 415,200 | 20.10 | 12.39 | 4.937 / 31.219 |
| Excel-DNA / 100 | Not supplied | 3.09 | -0.78 | 4.559 / 9.659 |
| xlfn / 1000 | 4,152,000 / 4,152,000 | 21.40 | 9.69 | 5.410 / 7.484 |
| Excel-DNA / 1000 | Not supplied | 17.86 | 5.79 | 5.976 / 8.478 |

Growth is from the post-warmup baseline to the final measured iteration;
1 MiB = 1,048,576 bytes. These instrumented timings are diagnostic observations,
not framework performance ratios. Excel-DNA reports cumulative managed
allocations, which cannot supply the Rust allocated/freed comparison.
Balanced requested-byte counters do not establish a flat live heap or rule
out native process-heap allocation, Excel retention, or allocator effects.

In the 1,000-iteration runs, private bytes grew by only 1.29 MiB (xlfn) and
0.23 MiB (Excel-DNA) over iterations 501–1000. First/last 100-iteration median
times rose from 4.946 to 5.745 ms for xlfn and 5.211 to 6.394 ms for Excel-DNA,
although each run's final individual iteration was faster than its first.
Single first/last ratios therefore conceal the block-level drift. These
observations do not isolate a framework-specific leak or latency cause.

The slowest xlfn / 100 iteration took 65.613 ms: 7.434 ms in `CalculateFull`
and 58.179 ms in completion-state observation. All measured iterations in
all four runs used exactly one completion poll. That long observation is a
COM query/scheduling interval, not evidence of repeated polling sleeps or
proof that Excel was still calculating throughout it.

The measured runner used range calculation for warmup and `CalculateFull`
for measurement. The updated runner warms `CalculateFull` as well and records
`warmup_calculation`; its effect requires a fresh live run. The earlier large
RSS increase occurred on a different Excel build and code revision, so its
absence here is not evidence that a leak was fixed. Event removal still needs
the interactive, pinned-module qualification below; accepting zero solely to
eliminate Quarantine would assert host behavior not established by this run.

## Public contract and remaining uncertainty

Microsoft's [xlEventRegister documentation](https://learn.microsoft.com/en-us/office/client-developer/excel/xleventregister)
specifies a string procedure name, an integer event selector, and an integer
result greater than zero for success. Zero reports failure. Historical Excel
comparison evidence, recorded in commit `8ba76798ec1bffb8f52eac96b777986a803e93a7`,
included `0x9d380001` (signed `-1657274367`) for registration. xlfn therefore
accepts nonzero integer registration acknowledgements and preserves their bits.
Zero and other result types remain rejected. This is an explicit accommodation
of observed host behavior, not a claim that Microsoft documents negative values.

The positive-only check reintroduced in `7b747c68` rejected that observation.
Because calculation events are registered before worksheet functions, that
rejection aborts `xlAutoOpen` even when the first requested UDF is synchronous
(`BENCH.ID` in S01). A subsequent formula can then display `#NAME?`. This is a
known failure path; a new `#NAME?` report without its startup log does not prove
that the same path caused it. Inspect the runner's `startup_log` and
`registration_diagnostics` fields to distinguish other load/registration faults.

The current removal operation passes `xltypeNil` as the procedure and the
event selector as `xltypeInt`. That removal convention is **not specified by
the public xlEventRegister page**. A positive acknowledgement alone therefore
does not establish that Excel stopped calling the handler. The convention
needs observed registration/removal behavior on each deployed configuration.
The signed registration accommodation does not apply to removal: a negative
removal acknowledgement still retains the pending event and unload hold.

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

### Runnable interactive probe

The CI artifact set now includes `event_probe/event-probe.xll`, built from
[`probe/`](probe/). It uses the repository's physical Excel ABI, but has its own
handlers, async delay workload and raw C API logging. It does not change the
production `RegistrationHost` decoder or discard production cleanup debt.
Windows pins the probe DLL until process exit **before** any handler is
registered. Each registration cycle uses different exported handler addresses,
so an old handler cannot be mistaken for the current generation.

Use a CI artifact produced **after this probe was added**, with a checkout at
the manifest's exact commit. Earlier artifacts, including run `40175450611`,
do not contain it. The existing `fetch-ci.ps1` verifies its hash and x86_64 PE
architecture along with the comparison XLLs. Install `pywin32` and `psutil` in
the same Python environment as the normal comparison runner.

From `benchmarks/excel-comparison` on Windows with 64-bit desktop Excel:

```powershell
python .\event_probe.py --artifacts C:\path\to\verified-artifacts --out event-probe-results
```

The output directory must be new. The runner creates a separate visible Excel
process and a temporary workbook; it refuses to modify an already-running
Excel process. It guides the following sequence (two removal cycles by default):

1. A programmatic calculation control, recorded separately.
2. In Excel, press **F9** and wait for cell A6 to become `1`; return to the
   terminal and press Enter. This must produce CalculationEnded.
3. Press **F9**, then **Esc** while the 10-second async calculation is pending;
   return to the terminal and press Enter. This must produce CalculationCanceled
   followed by CalculationEnded.
4. The runner calls nil-procedure removal from a registered XLL command, captures
   both raw acknowledgements, then asks for the same normal/cancel operations.
5. It re-registers new handler addresses and repeats both positive controls,
   then repeats steps 4–5 once more.

Do not use the COM console, VBA, or another automation tool for the requested
F9/Esc actions: these are the interactive stimulus. Do not close Excel until
the script finishes. No additional comparison XLL needs to be loaded. All
workers must drain before event registration changes. After removal, a canceled
worker may remain alive for the rest of its 10-second delay because its handler
is absent; the probe preserves Excel's late `xlAsyncReturn` response. For this
case the analyzer requires `xlretInvAsynchronousContext` (`256`) to corroborate
the cancellation. A different response stays inconclusive, even if the operator
pressed Esc.

Return **both `report.json` and `probe.jsonl`** from the output directory. The
report records the CI manifest, XLL hash, runner commit, process identity, Excel
build/bitness/executable, phase counters, raw responses and trace hash. The raw
log preserves handler generations, callback order, async attempts and close.
Interrupted or failed experiments retain their partial evidence. The runner
closes only its own workbook/process; it never force-kills Excel. The pinned DLL
is released by Windows when that process exits.

Possible assessments are `no_callbacks_observed_after_removal`,
`callbacks_observed_after_removal`, and `inconclusive`. Missing positive controls,
missing workload execution, stale generations, or incomplete evidence cannot
pass. Even a complete pass establishes only the observed **command-context**
removal behavior on this build. Production `xlAutoClose` ordering, module unload,
task/ledger reclamation and close/reopen remain separate qualification cases.
Use this result to choose the next production change; do not infer that raw
integer zero always means successful removal.

### Full production qualification

Record one result for every exact Excel version, build, update channel, and
Excel process bitness distributed to. A record for one version or architecture
does not qualify another. Include both 32-bit and 64-bit Excel only when both
are distribution targets; each needs its matching native XLL. Identify Excel
bitness from Excel's About dialog and the running EXCEL.EXE architecture,
rather than the operating-system or Python bitness.

Use a fresh interactive Windows Excel process and a trusted instrumented XLL
from the candidate commit. Retain the XLL's SHA-256 and the instrumented
source/build diff. The existing comparison runner's version/build and XLL
metadata can accompany the record. With `--shutdown-diagnostics`, failed or
indeterminate removals include raw results; successful registration results
and interactive callback counters still need a dedicated probe. Worksheet
results alone do not prove nil-procedure removal.

1. Capture the actual `xlEventRegister` arguments, callback transport status,
   raw result `xltype`, signed integer value, and raw integer bits for both
   exported handlers: `__xlfn_calculation_canceled` and
   `__xlfn_calculation_ended`. Capture signed values and their complete raw bits;
   distinguish the documented positive result from the signed accommodation.
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
The following test coverage is local mock evidence only: signed nonzero
registration acceptance, zero/wrong-type rejection, indeterminate event
mutations, and retention/retry after a negative removal acknowledgement.
