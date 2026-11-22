# Windows desktop Excel qualification

Run each gate on both 32-bit and 64-bit desktop Excel in a disposable Windows
VM or dedicated test account with no user workbooks or other add-ins running.
Match the Rust target to **Excel's** bitness (Help/About Excel), not Windows'
bitness. Keep normal add-in trust, signing, and endpoint protections in place.
Load only an approved package from a dedicated local test directory using the
normal Excel add-in UI or the isolated runner below. If policy blocks loading,
record the exact error as **blocked**; do not weaken policy.

## 1. Artifact and native loader

From the candidate checkout, use PowerShell and the Rust/MSVC tools required
by the repository. Run once per Excel bitness:

```powershell
$Target = 'x86_64-pc-windows-msvc' # Use i686-pc-windows-msvc for 32-bit Excel.
$Manifest = 'tests/xlfn-e2e-fixture/Cargo.toml'
cargo run --locked --package cargo-xlfn -- package --manifest-path $Manifest `
  --target $Target --out tests/xlfn-e2e-fixture/package --locked
./tests/fp12-multi-image/qualify.ps1 -Target $Target
```

Check each exit status separately. Use
`tests/xlfn-e2e-fixture/package/win-x64/FrameworkFixture.xll` or the
`win-x86` counterpart. Keep the whole package together and save the
`build-manifest.json`, XLL SHA-256, and build transcript. Use a new dedicated
output root if the old one contains unrelated files. The default Rust CRT
policy is static; retain the requested and observed policy/imports from the
manifest. Windows CI also runs `cargo xlfn check` across static, dynamic, and
inherit. For a non-default shipping policy, package with the same `--crt`
and qualify those actual bytes; a dynamic build needs the matching approved
Microsoft VC runtime. Do not bypass the package import checks or copy
arbitrary runtime DLLs beside the XLL.

Save the [native FP12 loader](../fp12-multi-image/README.md) transcript or the
exact candidate CI job and target. It proves two independent DLL images share
the quota and leave one persistent anchor through true unload and reload in
the **test loader**, not that Excel unloads an XLL.

## 2. Workbook date-system runner

Follow the [isolated date-system runner](../excel-date-system/README.md) using
the XLL above and a new evidence JSONL path. Require all 702 cases and a
terminal `passed` summary with owned-process cleanup complete. Run this
separately from the manual worksheet checks. Workbook results and process
exit are not evidence of physical image unload.

## 3. Logical lifecycle in desktop Excel

Start a fresh test Excel instance and use a new scratch workbook. Load the
packaged XLL via **File → Options → Add-ins → Excel Add-ins → Go → Browse**.
Examples below use English formulas and comma separators; use the local
argument separator where needed. Record build, bitness, PID, formulas,
observed results, elapsed times, and any Excel error values.

1. **Registration and async.** `=FRAMEWORK.VERSION()` returns fixture version
   `0.2.0`. Enter `=FRAMEWORK.ASYNC.ADD(2,3)`; after calculation settles it
   returns 5. Change it to `=FRAMEWORK.ASYNC.ADD(-2,9)`; it must settle to 7.
   Repeat after re-enabling the add-in. This fast fixture verifies native
   delivery but cannot reliably hold a calculation open to prove cancellation
   or completion racing removal.
2. **Handle generation.** Set A1 to `=FRAMEWORK.HANDLE.CREATE(17)` and A2 to
   `=FRAMEWORK.HANDLE.VALUE(A1)`; expect 17. Paste A1 **as a value** in another
   cell to preserve the old token without recalculating it. After disabling and
   re-enabling the add-in in the same Excel PID, a fresh token must work and
   `FRAMEWORK.HANDLE.VALUE` applied to the preserved token must return
   `#N/A`. Do not mistake failure to paste the original token for a successful
   stale-token check.
3. **RTD subscribe, publish, disconnect.** In three cells enter
   `=FRAMEWORK.RTD.FIXTURE(1)`, `=FRAMEWORK.RTD.FIXTURE(2)`, and
   `=FRAMEWORK.RTD.FIXTURE(3)`. Re-evaluate
   `=FRAMEWORK.RTD.ACTIVE()` until it reads 3. Then enter
   `=FRAMEWORK.RTD.PUBLISH(3)` in a scratch cell. For the observed publish
   result `n`, the topics should settle to `100*n+1`, `100*n+2` and
   `100*n+3`. The publish formula is volatile: capture the resulting batch
   before triggering another recalculation, then clear the publish formula.
   Remove all three subscriber formulas, allow Excel's RTD interval to elapse,
   and re-evaluate the active count; require 0. Record the wait and any
   nonzero count rather than assuming disconnect on formula deletion.
4. **Remove and reopen.** With the scratch workbook still open, uncheck the
   fixture in the Excel Add-ins UI, then re-enable it in the same PID. Check
   fresh async/handles and repeat the RTD connect/publish cycle. Separately do
   one owned-session normal exit with RTD subscriptions still present.
   Retain unexpected prompts, crashes, hangs, stale updates, or registration
   failures as evidence. Do not terminate a user's Excel process or call
   `FreeLibrary` in Excel to force a pass.

Allow Excel's observed calculation/RTD update intervals and record the elapsed
time for timeouts. A pass here is evidence of worksheet behavior and logical
cleanup only, not proof of specific thread schedules or absence of native leaks.

## 4. Separate physical Excel unload gate

`FrameworkFixture.xll` uses the default logical policy; its module may be
retained intentionally. Removing/unchecking it, losing worksheet names,
closing a workbook, or exiting Excel does **not** satisfy this gate. A native
loader result cannot substitute for the host-specific check either.

A future separate test needs a reviewed candidate XLL explicitly opting into
`physical_unload` with the unsafe `PhysicallyUnloadableAddin` contract.
It must have drained all native execution sources and not have retained
framework roots (including calculation callbacks or COM references). Even
then, Excel may keep the image resident. In the dedicated Excel PID, capture
the module's full path and base before normal removal, then use an approved
read-only module observer or debugger unload event to show that exact image
has been unmapped **while the same PID is still alive**. Verify it is mapped
again following normal reload in that PID and confirm a known worksheet call.
An unobservable module, retained image, or changed PID is not a pass. To
qualify Excel's FP12 pointer lifecycle, also use a separately reviewed
FP12-returning XLL and test Excel's actual pointer/free callbacks across
recalculation and removal; this fixture does not export an FP12 result.

For each width and gate, retain UTC timestamps, candidate Git revision and
build provenance, Windows/Excel build, package manifest and hash, transcript,
JSONL or worksheet observations, and module evidence where applicable. Mark
each gate **pass**, **fail**, **blocked**, or **not run** independently. This
runbook records no execution in desktop Excel.
