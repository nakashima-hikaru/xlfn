# Post-calculation shutdown comparison, 2026-10-26

Status: **the intermittent Excel process survival remains unresolved**. Source
review identifies a substantial difference in the work done inside
`xlAutoClose`; it does not establish that this difference caused the reported
M01/10x10 and T01/ascii-8 failures. No new native Windows execution or failing
process dump was available for this review. The earlier opening-reentry fix is
separate from this post-calculation investigation.

## Source provenance

- xlfn reviewed at `5bd23bb1c73c00bcc7ae9b60f0e231c9c7ef3ee6`.
- The benchmark specifies `ExcelDna.AddIn.NativeAOT` **1.10.0-preview5**, targeting
  .NET 10 / win-x64, in [ExcelComparison.csproj](excel_dna/ExcelComparison.csproj).
- Retrieved the [official NuGet package][package]. Its SHA-256 was
  `d25940149c5a7ac7b19de09c8071b35dea3bc21cba9f41428171fc9eb9128460`.
  The Integration, Loader and ManagedHost DLLs each contain the informational
  version `1.10.0.5+363b5c05c47b2c00f55d3a9eea5cc0dfdf77dad4`.
- Reviewed that source revision. NativeAOT's [project][project] imports
  `ExcelDna.Host.Shared.vcxitems`; its lifecycle exports come from
  [Host.Shared/addin.cpp][native], not the classic .NET Framework
  `Source/ExcelDna/ExcelDna.cpp`. The [NativeAOT host][aot] calls the generated
  `Initialize` export, which leads to `InitializeNativeAOT` and the shared
  [managed loader][managed]. This is source-level tracing, not a claim of
  reproducible native-binary equivalence or inspection of the user's loaded XLL.

## Main difference: close and explicit removal

| Operation | Excel-DNA NativeAOT source | xlfn benchmark |
| --- | --- | --- |
| Ordinary `xlAutoClose` | Calls loader `XlAutoClose`, which runs COM-object collection; no `DeInitializeIntegration` call here | Calls `deactivate_addin`, entering the full terminal-removal pipeline |
| User add-in cleanup | `IExcelAddIn.AutoClose` is reached through `DeInitializeIntegration`, normally on `xlAutoRemove` or replacement open | Quiesce, cleanup and destruction run during terminal removal, including an ordinary close |
| UDF unregister/name cleanup | Performed by `UnregisterMethods` during deinitialization | Performed inside ordinary close as well |
| Explicit `xlAutoRemove` | Deinitializes, then sets the native `removed` flag | Runs terminal removal; a subsequent close observes the completed state |
| Native DLL retention | `LockModule` adds a loader reference; normal close releases it only after the `removed` path | Safe add-ins retain the residency lease; resident calculation events also prevent its release |
| `DllCanUnloadNow` | Shared native export returns `S_FALSE` | Returns `S_FALSE` while the residency lease is held |

Excel-DNA's fixture [AutoClose](excel_dna/Functions.cs) therefore need not appear
in a trace from ordinary `Application.Quit`, even when the native `xlAutoClose`
ran. The prior observation of missing `IExcelAddIn.AutoClose` is consistent with
this source path; it is not evidence that Excel omitted its native callback.

Excel-DNA's [COM collection loop][com] is managed-runtime housekeeping. It does
not imply that xlfn needs a GC call: native COM references must instead be
released by their owners. DLL residency and returning `S_FALSE` alone do not
distinguish the two implementations.

## xlfn operations requiring discrimination on the failing process

The path starts at
[`host_auto_close`](../../crates/xlfn/src/boundary/host.rs), then
[`remove_addin_inner_unchecked`](../../crates/xlfn/src/runtime/transactions.rs).
The following are possible blocking or failure sites, not confirmed defects.

1. **Export drain with COM dispatch.** On Windows with RTD/handles enabled,
   [`ExportIngress::seal_and_drain`](../../crates/xlfn/src/ingress.rs) makes two
   drain calls. Each uses
   [`drain_with_com_dispatch`](../../crates/xlfn/src/excel_rtd/windows/event.rs),
   which starts a scoped waiter and calls `CoWaitForMultipleHandles` on the
   lifecycle thread. This path is used even for synchronous workloads with no
   RTD server. It deliberately allows incoming COM dispatch while cleanup is in
   progress. The same-runtime lifecycle guard coalesces reentrant close/remove;
   it does not prove that every possible host interaction during that wait is
   benign. A failing stack in `CoWaitForMultipleHandles` must be paired with the
   waiter stack and outstanding ingress state; finding CoWait alone is not a
   deadlock diagnosis.
2. **Return and worker waits.** Return obligations are drained, async admission
   closes, tasks are cancelled/drained, and every built-in poller is joined.
   The fixture starts 32 pollers even for S01/M01/T01. Queue sealing wakes all
   pollers; source review did not find an omitted join or a demonstrated lost
   wake. With no async workload, a long task is a weaker explanation. A stack
   blocked in `ReturnTracker` or `Pool::close_and_join` would identify a different
   investigation from a process whose runtime has already reached `Closed`.
   [Microsoft documents][autofree] `xlAutoFree12` immediately after a worksheet
   return, on the same calculation thread and before its next cell; this review
   does not assume Excel normally postpones those frees until after close.
   M01 is matrix **input** followed by a numeric sum; T01 is string **input**
   followed by a numeric character count. Their successful outputs do not
   publish string/array return blocks. A retained string/array return therefore
   does not explain these cases merely because their workload labels mention
   matrices or strings.
3. **Excel C API mutations during Quit.** xlfn unregisters each UDF by numeric
   ID, evaluates the hidden name to check ownership, then deletes matching
   names. This is `xlfUnregister` Form 1 plus `xlfEvaluate`/`xlfSetName`, not
   DLL-wide Form 2 or a cleanup-time `xlfRegisterId` that could register again.
   Terminal callback errors stop further ordinary callbacks. Failed UDF
   detachment can quarantine the generation; name-cleanup debt alone is treated
   separately. Excel-DNA's ordinary close does not perform this sequence.
   Its explicit-removal [implementation][unregister] also uses a temporary
   command registration to work around Function Wizard metadata behavior.
   There is no evidence that copying that workaround fixes process survival.
4. **Later resources.** Diagnostic-worker joining and RTD module quiescence can
   also wait. However, registering `BenchRtdSource` during open does not create
   an Excel RTD COM server. `observe_subscription`/handle observation creates it
   when an RTD-dependent function runs. M01/T01 do not take that path, so an RTD
   update-callback reference leak is not the leading source-backed explanation
   for these two workloads.

[Microsoft's close contract][close] permits resource cleanup and function
unregistration from `xlAutoClose`; Excel unregisters functions after it returns
if the XLL did not. Consequently, xlfn doing more work here is **a strong
experimental distinction, not by itself an API-contract violation**. Likewise,
returning 1 from `xlAutoClose` is required by that contract and does not certify
xlfn's internal close phase. Changing that return value is not a repair.

## Targeted investigation: late events during unregistration

The specific hypothesis that a late calculation event reaches a stopped async
executor or reclaimed generation **during UDF unregistration** was not supported
by source review or the new callback-injection tests. Its priority is reduced;
this is not a resolution of the reported Excel process survival.

The current order is:

1. Seal generation publication when final removal begins. The lifecycle phase
   becomes `Closing` before the removal claim transitions module ingress, so a
   brief `Closing`/ingress-open projection is possible. Generation admission
   already rejects events in that interval.
2. Drain and seal module ingress, including calculation events admitted before
   the close request. Such events retain both ingress and generation permits
   through their operation.
3. Drain return obligations and stop producers, including the async executor.
4. Only then perform `xlfUnregister`, ownership checks with `xlfEvaluate`, and
   matching-name deletion with `xlfSetName`.

The generated calculation-ended/cancelled exports both use
[`ffi_boundary_void`](../../crates/xlfn/src/return_abi.rs), which requires module
and generation admission. A late event during step 4 returns at the sealed
module gate without reaching calculation state. The distinction between this
export gate and the Excel callback gate matters: cleanup still needs permission
to call Excel, while new calculation-event exports are already rejected.

Added [boundary tests](../../crates/xlfn/src/boundary/event_shutdown_tests.rs)
that exercise the real open/close pipeline and the calculation-event boundaries
with a scripted Excel C API:

- Eight combinations of close/remove and injection during `xlfUnregister`,
  `xlfEvaluate`, `xlfSetName`, or `xlFree`. Each injects both event types on the
  lifecycle thread and another thread. Events leave calculation identity
  unchanged, export ingress stays closed, the executor is stopped, and cleanup
  reaches `Closed`.
- Six combinations of an unregister/name callback returning `xlretAbort` or
  `xlretUncalced`, with events injected inside that call. No subsequent ordinary
  Excel callback is issued. A failed UDF detach remains quarantined; failed name
  cleanup alone retains metadata debt. Events are also inert after either
  terminal outcome.
- An already admitted calculation-ended event is paused before its transition.
  Close begins on the lifecycle thread, later events are rejected, and
  unregistration cannot proceed until the admitted event is released. At the
  actual unregister call, that event has advanced calculation identity exactly
  once, active exports are zero, and the executor is stopped.

The fixture uses 32 built-in pollers with all features enabled, matching the
benchmark's configured count. It first verifies that a calculation-ended event
does advance a live generation, so the late-event checks do not merely exercise
an inactive fixture. The two callback-injection tests also pass without default
features; cancellation and the paused async-transition case require async
features.

Local validation: the all-feature xlfn library suite passed **1,032 tests**, with
eight ignored; the three targeted tests passed again after strengthening the
unregister-state assertion. The two applicable tests passed with
`--no-default-features`. Formatting and all-feature library/test Clippy checks
also passed.

These are local tests with a simulated Excel callback, not native Windows/Excel
qualification or exhaustive scheduling proof. They do not model Excel's own
registration locks, COM dispatch, other add-ins, or the host's work after xlfn
cleanup. Those broader host interactions remain possible even though the
specific late-event access to xlfn's stopped state is guarded.

## Fit to the reported failure and next discriminator

The runner's reported error, `Excel survived worker teardown and required
forced termination`, is assigned after the worker has completed, rather than
being the worker-timeout error. On a successful worker path, workbook cleanup,
`Application.Quit`, release of Python references, and worker exit have already
occurred. This weakens an explanation consisting only of an ordinary synchronous
hang inside Quit. It does not reveal Excel's remaining thread stacks or exclude
deferred/reentrant shutdown. The five-second deadline records process survival,
not proof of an infinite hang.

The existing [resident-event observations][observations] are from a different
build (`9c72a05`) and successful L02 cases: xlfn reached `Closed` in about 13 ms
while natural process exit followed Quit by 2.39/4.07 seconds. They demonstrate
that runtime-close duration and process-exit duration can differ substantially.
They do not explain or clear the newer M01/T01 failures.

Use the existing `--dump-on-failure` mode on the original small case set. Keep
the failure status even if Excel exits during capture. The first decisive split
is whether the failing process is still in xlfn cleanup, in a reentrant COM
chain, or already past xlfn cleanup and blocked in Excel/another component.
Successful instrumented reruns cannot answer that split.

A subsequent controlled experiment can move **the same** xlfn close operation
before Quit, while Excel and a bootstrap workbook are still alive, and compare
against the existing order. DLL-wide `UNREGISTER` invokes `xlAutoClose`, not an
equivalent Excel-DNA explicit-removal path, so such a run must be labeled as a
timing/order experiment and must not be merged into normal benchmark results.
Do not skip cleanup or increase the pass deadline and call that a fix. Reverse
implementation order as a separate control: the normal runner currently runs
xlfn before Excel-DNA within each case.

[package]: https://api.nuget.org/v3-flatcontainer/exceldna.addin.nativeaot/1.10.0-preview5/exceldna.addin.nativeaot.1.10.0-preview5.nupkg
[project]: https://github.com/Excel-DNA/ExcelDna/blob/363b5c05c47b2c00f55d3a9eea5cc0dfdf77dad4/Source/ExcelDna.Host.NativeAOT/ExcelDna.Host.NativeAOT.vcxproj#L63-L65
[native]: https://github.com/Excel-DNA/ExcelDna/blob/363b5c05c47b2c00f55d3a9eea5cc0dfdf77dad4/Source/ExcelDna.Host.Shared/addin.cpp#L127-L179
[aot]: https://github.com/Excel-DNA/ExcelDna/blob/363b5c05c47b2c00f55d3a9eea5cc0dfdf77dad4/Source/ExcelDna.Host.NativeAOT/host.cpp#L56-L76
[managed]: https://github.com/Excel-DNA/ExcelDna/blob/363b5c05c47b2c00f55d3a9eea5cc0dfdf77dad4/Source/ExcelDna.Loader/XlAddIn.cs#L239-L354
[com]: https://github.com/Excel-DNA/ExcelDna/blob/363b5c05c47b2c00f55d3a9eea5cc0dfdf77dad4/Source/ExcelDna.Loader/LoaderHelpers.cs#L49-L59
[unregister]: https://github.com/Excel-DNA/ExcelDna/blob/363b5c05c47b2c00f55d3a9eea5cc0dfdf77dad4/Source/ExcelDna.Loader/XlRegistration.cs#L203-L243
[close]: https://learn.microsoft.com/en-us/office/client-developer/excel/xlautoclose
[autofree]: https://learn.microsoft.com/en-us/office/client-developer/excel/xlautofree-xlautofree12
[observations]: event-qualification/l02-resident-events-2026-10-25.json
