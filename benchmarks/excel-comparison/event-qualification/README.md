# Calculation-event lifetime and qualification

Status on 2026-10-25: **production uses module-lifetime event registrations;
single-generation close with retained events was observed on Excel 16.0 build
20527.0 (64-bit). Same-process close/reopen remains unverified**. The earlier
nil-procedure removal experiment failed on this Excel build. Local tests,
cross-compilation and formal traces do not replace the remaining live cases.
The current evidence record is [records.json](records.json); it includes the
historical shutdown observations and removal counterexample, plus four scoped
production-close observations from the revised build with and without
allocation counters.

## Production lifetime contract

Calculation-event registrations belong to the loaded module's static runtime,
not to an individual open generation. The runtime acquires its DLL residency
reference before the first native registration and records each selector in
a bounded registration slot. Confirmed registrations are reused across logical
close, rollback and reopen; they are not registered a second time for a new
generation. A successful native registration remains recorded even if freeing
its result fails. In-flight or indeterminate registration retains the slot and
the lease, and prevents blind re-registration. A rejected operation may be
retried only when the registration layer establishes that it was not applied.

Production no longer calls nil-procedure removal or `xlfUnregister` on event
tokens. Closing seals external ingress and generation admission before draining
work. A later native notification can enter the resident trampoline, but cannot
access the stopped executor or reclaimed generation. Logical close can therefore
complete resource disposal and reach `Closed` while the native handlers and DLL
reference remain. Reopen publishes a new generation behind the same handlers.

`PhysicallyUnloadableAddin` is an opt-in to possible lease release, not a promise
of physical unload. Release requires zero native event slots, including
in-flight and indeterminate slots, in addition to the existing shutdown
conditions. Registration reservation and lease-release checks use the same
ownership lock. Retained events keep the DLL resident until process exit.
Shutdown traces report `residentEventRegistrations` and `moduleResidencyHeld`;
they do not emit an event-unregistration certificate for logical close.

This design was informed by source review of
[Excel-DNA's event subscriptions](https://github.com/Excel-DNA/ExcelDna/blob/f484749260d5cc43c35f7aea5f7a0b0f8f74a78f/Source/ExcelDna.Integration/ExcelAsyncUtil.cs#L223-L301)
and [native loader](https://github.com/Excel-DNA/ExcelDna/blob/f484749260d5cc43c35f7aea5f7a0b0f8f74a78f/Source/ExcelDna/ExcelDna.cpp#L352-L367).
Excel-DNA drops internal subscribers, ignores the removal call's returned value,
and gates forwarding through its loader state. Its source does not establish
that Excel detached the native handler. The reviewed
[xlOil registration/close implementation](https://github.com/cunnane/xloil/blob/920c71f5e33e8b8ab715dc1d72d1216b16e6eacc/include/xloil/XllEntryPoint.h#L68-L114)
also does not issue a corresponding native event-removal call. These are source
observations that informed xlfn's lifetime design, not live qualification of
either OSS or the revised xlfn implementation.

The [2026-10-26 shutdown comparison](../shutdown-comparison.md) separately
traces the benchmark's NativeAOT 1.10.0-preview5 source revision. It distinguishes
ordinary close from explicit removal and records the unresolved process-exit
failure; the event-policy comparison above does not establish identical teardown.

## Interactive removal counterexample, 2026-10-25

[Selected native evidence](probe-2026-10-25-nil.json) preserves all 98 native
events and the 11 phase ranges from the supplied report/trace. Their original
SHA-256 values are retained. The trace hash matches the report and both copies
of the events agree. All four recorded Python source hashes match commit
`157fcf162d3a2be81268cc97895b825374d9cc5f` after LF-to-CRLF conversion.
The XLL came from CI run `40184459490`, attempt 1, with SHA-256
`6d9436408dadf0c8466281a6e0846770501bf67117fe91882dd814db92efc233`.
Excel reported version 16.0, build 20527.0, 64-bit; its update channel was not
captured. This is a result for that observed configuration, not all Excel builds.

| Stage | Ended callbacks per normal calculation | Canceled / Ended callbacks per interruption | Handler generations |
| --- | ---: | ---: | --- |
| Initially registered | 1 | 1 / 1 | 1 |
| First nil removal | 1 | 1 / 1 | 1 |
| First re-registration | 2 | 2 / 2 | 1, 2 |
| Second nil removal | 2 | 2 / 2 | 1, 2 |
| Second re-registration | 3 | 3 / 3 | 1, 2, 3 |

All four removal calls returned `xltypeInt 0`, transport success and successful
result cleanup. Both event kinds continued to invoke their old handler addresses.
Every phase started exactly one async task and ended with no active workers;
normal calculation returned worksheet value 1, and cancellation preceded the
Ended events. The process exited naturally. **Nil-procedure removal did not
detach these handlers.** Re-registration added further live handlers in this
probe; it did not replace the old generation.

The original analyzer labeled this report `inconclusive` because programmatic
calculation also raised an Ended callback and old generations remained callable.
The latter is failure evidence, not missing evidence. The revised analyzer
classifies this same immutable input as `callbacks_observed_after_removal`;
it records programmatic behavior separately and still rejects inconsistent
counters, incomplete call sequences and missing workload observations. The
programmatic observation does not by itself qualify interactive cancellation.

The historical zero-result rejection and unload hold were justified by this
counterexample. Current production keeps those event roots resident instead
of attempting their removal. The standalone probe also retains an optional
experiment that passes the signed registration ID returned by `xlEventRegister`
to `xlfUnregister` as `xltypeNum`. Microsoft's
[xlfUnregister documentation](https://learn.microsoft.com/en-us/office/client-developer/excel/xlfunregister-form-1)
covers IDs from `xlfRegister`, so its applicability to event IDs is a hypothesis
to test, not an established production contract. That experiment is not a
prerequisite for the production lifetime fix and has no qualified live result.

## Revised production L02 observations, 2026-10-25

[Selected evidence](l02-resident-events-2026-10-25.json) preserves summaries,
memory segments, block endpoints and all lifecycle event details from four
supplied records. Both runner and XLLs report commit
`9c72a05f97f9c27a8b2ec364ed12daa2ad596ff3`, CI run `40190045107`, attempt 1.
The xlfn XLL SHA-256 is
`4a29603f668740ed538a5a371e050d2779395605a229b2f4e378c50cfd5a11c2`.
Excel reports version 16.0, build 20527.0, 64-bit, with 1,000 cells, one
calculation thread and manual calculation. The update channel is unavailable.
All four runs record `CalculateFull` warmup and allocation diagnostics.

Both xlfn cases record `GenerationReclaimed`, `FinishClose`, then
`xlAutoClose.return` with phase `Closed`, status 1,
`resident_event_registrations: 2` and `module_residency_held: true`.
Neither contains a Quarantine checkpoint or registration-removal failure
record. Close-body durations were 13.15 ms (100 repetitions) and 13.44 ms
(1,000 repetitions). All four Excel processes exited naturally with code 0
and no forced kill; xlfn's Quit-to-exit intervals were 4.07 s and 2.39 s.
This establishes the observed terminal path and the runtime's reclamation and
residency diagnostics for these cases. It does not establish physical DLL
unload, late-handler behavior, cancellation, or reuse across same-process opens.
The Excel-DNA managed AutoClose callback was not observed; that alone says
nothing about native teardown completeness.

| Implementation / repetitions | Rust allocated / freed bytes | RSS growth (MiB) | Private bytes growth (MiB) | p50 / p99 (ms) |
| --- | ---: | ---: | ---: | ---: |
| xlfn / 100 | 415,200 / 415,200 | 4.12 | 1.54 | 3.846 / 6.226 |
| Excel-DNA / 100 | Not supplied | 7.49 | 3.57 | 4.873 / 7.124 |
| xlfn / 1000 | 4,152,000 / 4,152,000 | 45.18 | 30.12 | 4.026 / 6.411 |
| Excel-DNA / 1000 | Not supplied | 23.73 | 11.73 | 4.662 / 7.692 |

Growth is from the post-warmup baseline to the final measured iteration;
1 MiB = 1,048,576 bytes. Instrumented timings do not establish performance
ratios. Balanced Rust requested-byte counters exclude native process-heap
allocations, Excel retention and allocator overhead; they are not a live-heap
measurement or proof that no leak exists.

Memory growth remains unresolved. In the xlfn 1,000-iteration case, iterations
501–1000 added 28.75 MiB RSS and 16.98 MiB private bytes. The first and last
100-iteration median times were 3.871 and 4.617 ms; the final iteration alone
was faster than the first, so the endpoint ratio hides the block-level drift.
Excel-DNA added 17.60 MiB RSS and 7.28 MiB private bytes in its latter half.
The slowest xlfn iteration took 19.886 ms, of which 16.204 ms was the single
completion-state COM query. Every iteration in every case used one poll.

The prior xlfn 1,000-iteration observation grew by 21.40 MiB RSS and 9.69 MiB
private bytes. The new run therefore does not support a memory-growth fix.
Code, warmup and process baselines differ, so these single observations also
do not isolate a regression caused by the event-lifetime change. The next
lifecycle qualification requires interactive normal/canceled calculations and
close/reopen cycles while one Excel process remains alive, with native
registration and admitted-generation observations. L02 does not provide them.

## L02 with allocation counters disabled, 2026-10-25

[Selected evidence](l02-no-allocation-counters-2026-10-25.json) records the
follow-up without `--repeat-diagnostics`. All four cases have
`repeat_diagnostics: false` and no allocation-window metrics. They match the
previous run's reported host, Excel version/build, CI run, runner commit,
per-implementation XLL hashes, case parameters, calculation mode and thread
count. `CalculateFull` warmup and shutdown diagnostics remain enabled. These
are separate process runs about 30 minutes later, not randomized paired trials;
differences in their timings cannot measure counter overhead precisely.

| Implementation / repetitions | RSS growth (MiB) | Private bytes growth (MiB) | p50 / p99 (ms) |
| --- | ---: | ---: | ---: |
| xlfn / 100 | 13.41 | 8.20 | 3.643 / 7.740 |
| Excel-DNA / 100 | 4.72 | 3.34 | 4.075 / 5.440 |
| xlfn / 1000 | 42.27 | 31.18 | 4.061 / 5.856 |
| Excel-DNA / 1000 | 21.02 | 11.58 | 4.448 / 8.031 |

Growth is from the post-warmup baseline. The xlfn 1,000-iteration growth is
similar with counters enabled (45.18 MiB RSS, 30.12 MiB private bytes) and
disabled (42.27 MiB RSS, 31.18 MiB private bytes). Its latter 500 iterations
still add 26.24 MiB RSS and 16.48 MiB private bytes. The accumulation therefore
does not require enabled allocation counters. The counts from the earlier
instrumented run do not become a live-allocation measurement for this run.

Absolute baselines matter when comparing implementations: xlfn private bytes
start at 112.98 MiB and end at 144.16 MiB; Excel-DNA starts at 127.76 MiB and
ends at 139.34 MiB. The 19.60 MiB difference between their growth figures is
not a 19.60 MiB difference between final private footprints (which differ by
4.81 MiB). Final RSS is 255.07 MiB for xlfn and 261.07 MiB for Excel-DNA.
These observations neither identify allocation owners nor establish a leak.

The first/last 100-iteration medians increase from 3.647 to 4.225 ms for xlfn
(15.8%) and 4.165 to 4.739 ms for Excel-DNA (13.8%). Drift is present in both
implementations in this run; these data do not isolate its cause. The slowest
xlfn 1,000-iteration sample takes 19.755 ms, including 15.449 ms in its single
completion-state COM query. All iterations in all four cases use one poll.

Both xlfn cases again record `GenerationReclaimed`, `FinishClose` and `Closed`,
with two resident event registrations and the module lease held. Close bodies
take 13.23 and 14.38 ms. All four Excel processes exit naturally with code 0,
without forced termination or COM retries. Same-process close/reopen and
interactive cancellation remain outside this test's scope. Further memory
attribution needs observations of outstanding native allocations and their
call sites, including a control for Excel/COM activity, rather than interpreting
process totals as xlfn-owned allocations.

## Historical L02 observations, 2026-10-25

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
absence here is not evidence that a leak was fixed. These records describe the
old removal path; they do not validate resource reclamation or repeated opens
under the new lifetime design. Accepting zero solely to eliminate Quarantine
would have asserted host behavior that this run did not establish.

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

The historical removal operation passed `xltypeNil` as the procedure and the
event selector as `xltypeInt`. That removal convention is **not specified by
the public xlEventRegister page**. A positive acknowledgement alone therefore
does not establish that Excel stopped calling the handler. The convention
would need observed registration/removal behavior on each deployed
configuration before any future physical-removal path could rely on it.
Current production does not rely on this convention.

Microsoft's [Handling Events documentation](https://learn.microsoft.com/en-us/office/client-developer/excel/handling-events)
describes CalculationCanceled followed by CalculationEnded for user
interruption, and states that these events are not raised during programmatic
recalculation. A COM-only calculation run cannot establish the interactive
event sequence. Cancellation remains best effort.

An indeterminate registration result still cannot certify a usable event
subscription. The module keeps the possible native callback root and DLL
lease, logical cleanup can reclaim the uncommitted generation behind closed
admission, and a later open must not blindly repeat that registration. This
separates host registration uncertainty from generation-resource reclamation.

## Qualification gate

### Runnable interactive probe

This standalone probe reproduces and investigates native **removal** semantics.
It is retained for research and comparison with historical evidence. It does
not exercise production's module-lifetime registry and is not required to
qualify that registry's logical close/reopen behavior.

The CI artifact set now includes `event_probe/event-probe.xll`, built from
[`probe/`](probe/). It uses the repository's physical Excel ABI, but has its own
handlers, async delay workload and raw C API logging. Its deliberately changing
handler generations are independent of production's persistent handlers.
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

For an optional investigation of the unqualified registration-ID hypothesis,
use an artifact containing that method and a fresh process/output directory:

```powershell
python .\event_probe.py --artifacts C:\path\to\verified-artifacts --removal-method unregister-id --out event-probe-id-results
```

The default `--removal-method nil` retains the original experiment for direct
reproduction. `unregister-id` records the exact signed token, numeric argument,
transport, return value and cleanup for both selectors. A positive return alone
does not establish callback removal. The analyzer checks the target against
the preceding registration token and uses the same interactive controls.
Run `40184459490` supports only the original nil experiment.

The output directory must be new. The runner creates a separate visible Excel
process and a temporary workbook; it refuses to modify an already-running
Excel process. It guides the following sequence (two removal cycles by default):

1. A programmatic calculation control, recorded separately.
2. In Excel, press **F9** and wait for cell A6 to become `1`; return to the
   terminal and press Enter. This must produce CalculationEnded.
3. Press **F9**, then **Esc** while the 10-second async calculation is pending;
   return to the terminal and press Enter. This must produce CalculationCanceled
   followed by CalculationEnded.
4. The runner calls the selected removal method from a registered XLL command,
   captures both raw acknowledgements, then asks for the same normal/cancel operations.
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
`callbacks_observed_after_removal`, `stale_callbacks_after_reregistration`, and
`inconclusive`. Missing positive controls, missing workload execution or
inconsistent evidence prevent a conclusive assessment. Old handler generations
are retained as failure evidence; unexpected programmatic callbacks are recorded
as observations rather than masking an interactive counterexample.
Even a complete pass establishes only the observed **command-context**
removal behavior on this build. Production `xlAutoClose` ordering, module unload,
task/ledger reclamation and close/reopen remain separate qualification cases.
Do not infer that raw integer zero always means successful removal or use
this standalone probe to certify production generation reclamation.

### Full production qualification

Record one result for every exact Excel version, build, update channel, and
Excel process bitness distributed to. A record for one version or architecture
does not qualify another. Include both 32-bit and 64-bit Excel only when both
are distribution targets; each needs its matching native XLL. Identify Excel
bitness from Excel's About dialog and the running EXCEL.EXE architecture,
rather than the operating-system or Python bitness.

Use a fresh interactive Windows Excel process and a trusted instrumented
**production** XLL from the candidate commit. Retain the XLL's SHA-256 and the
instrumented source/build diff. The existing comparison runner's version/build
and XLL metadata can accompany the record. Its shutdown diagnostics alone do
not capture every native registration and event entry; add the instrumentation
needed to observe the following cases. A standalone removal probe, worksheet
results, or natural process exit cannot substitute for this evidence.

1. Capture the actual `xlEventRegister` arguments, callback transport status,
   raw result `xltype`, signed integer value, and raw integer bits for each
   registered handler. Record residency acquisition before registration and
   the module registry's confirmed, in-flight, or indeterminate state. Capture
   signed values and their complete raw bits; distinguish the documented
   positive result from the signed accommodation.
2. Observe both native handler entry and admitted generation work, with
   invocation/generation identifiers. Exercise normal interactive calculation,
   user cancellation while async calls are pending, and completion after
   cancellation. Record ordering, outstanding tasks, and whether late results
   reached worksheet cells. Use programmatic recalculation as a separately
   recorded control.
3. Close or explicitly remove the add-in while Excel remains running. Check
   admission closure, task/callback drain, generation-state disposal, and
   `Closed` publication. Confirm that no event-removal API is called and that
   resident native event slots retain the DLL reference. Late native handler
   entries are allowed; accessing the closed generation or restarting its
   executor is a failure.
4. Reopen the same loaded module and repeat normal/canceled calculations across
   multiple close/reopen cycles. Confirm that each confirmed selector has only
   one native registration call for this static runtime, that each expected
   event reaches the current generation once, and that no work reaches disposed
   generations. Preserve the registration counts rather than inferring reuse
   solely from worksheet values.
5. Exercise partial-open failure, registration-result cleanup failure, and
   indeterminate registration where an instrumented test can produce them.
   Confirm that known registrations survive rollback without duplicate calls,
   indeterminate slots prevent blind retry, generation resources can be
   disposed behind closed admission, and the module remains resident.
6. With `PhysicallyUnloadableAddin`, confirm that a retained native event slot
   still blocks lease release after successful logical close. A zero-event
   configuration must separately satisfy all existing teardown and user-code
   quiescence obligations before lease release is eligible. Do not infer
   physical unload from `Closed`, an `xlAutoClose` return, or Excel process exit.

Any required case without evidence keeps that configuration **unverified**;
a contradictory observation marks its tested scope **failed**. Keep the XLL
resident, or end the whole test Excel process, whenever a native callback root
may remain. Record actual observations before making any host-compatibility
claim. The current production implementation's new native qualification is
pending; the historical failed-removal records remain valid for their original
commit and scope.

## Record contents

Append real runs to `observations` in [records.json](records.json), preserving
earlier runs. Every observation must identify its scope and contain:

- UTC timestamp, operator, Windows version, Excel product/version/build/channel,
  explicit Excel process bitness, and process executable identity;
- candidate commit, XLL target architecture and SHA-256, instrumentation diff,
  calculation mode, multithreaded calculation settings, and test inputs;
- native registration calls with procedure/event arguments, transport status,
  raw result type, signed integer and raw bits, and result cleanup;
- normal/canceled/programmatic event traces, generation identifiers, repeated
  close/reopen registration counts, task/state disposal, residency observations,
  and late-result checks for production-lifetime qualification;
- actual removal calls and post-removal counter observations when the declared
  scope is the standalone removal experiment; these are not required events
  in a production logical-close trace;
- evidence-file paths and SHA-256 values, failed or omitted cases, and a final
  `verified`, `failed`, or `unverified` verdict for that scope.

Use `verified` only after all required observations for that exact configuration
and scope pass. Leave unavailable values absent or null. Missing required
evidence prevents `verified`, but does not erase a directly observed failure;
record a counterexample as `failed` with its actual tested scope. Do not turn
a planned configuration into an observed run. Mock ABI tests, lifecycle tests,
and formal trace checks establish local implementation properties only; they
do not qualify native Excel registration, cancellation, close/reopen, memory
reclamation, or physical unload on a deployed build.
