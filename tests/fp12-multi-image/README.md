# Windows multi-image FP12 quota qualification

Run on Windows (with the corresponding MSVC Rust targets installed):

```powershell
./tests/fp12-multi-image/qualify.ps1 -Target x86_64-pc-windows-msvc
./tests/fp12-multi-image/qualify.ps1 -Target i686-pc-windows-msvc
```

Each invocation starts a fresh loader process with two differently named copies
of one test DLL, so each DLL has its own linked copy of the production
`xlfn-host` code and an independent attachment cache. The first attachment
retains a small fixture allocation. The second must see its published bytes
and enforce the exact remaining capacity. The loader checks consumed handles
and tests repeated reloads, including reopening the section after all fixture
DLLs have been physically unmapped.

The native loader proves that the images are distinct and that `FreeLibrary`
really unmapped each image by querying a former export address via
`GetModuleHandleExW(FROM_ADDRESS | UNCHANGED_REFCOUNT)`. It also queries the
shared section via
[`NtQueryObject(ObjectBasicInformation)`](https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntqueryobject):
exactly one persistent section handle must remain besides the probe's temporary
handle. The probe closes before any unload and cannot itself anchor the name.
This native object query is fixture-only; production does not depend on it.

This verifies the **host process accounting protocol**, not Excel's use of an
FP12 result pointer or Excel's add-in unload behavior. It does not launch
Excel or open a workbook. Actual 32/64-bit Excel qualification remains a
separate gate.

For the separate Excel gates, see the
[Windows desktop Excel qualification runbook](../excel-qualification/README.md).
