# Workbook date-system qualification

**Native Excel gate: pending.** The runner and fixture have been prepared and
checked locally. No Windows desktop Excel execution evidence is included here.
Mocked callbacks and Windows cross-compilation do not satisfy this gate.

`run.py` loads the `xlfn-e2e-fixture` XLL into an isolated
`DispatchEx("Excel.Application")` instance. It creates three temporary workbooks
and closes only those workbooks, then requests exit of its own Excel instance.
It does not attach to an existing Excel session or save user workbooks.
If the new instance already contains startup workbooks, the runner refuses to
proceed and releases its COM references without closing those workbooks or
requesting Excel exit. That unverified instance may remain open for inspection.

The fixture provides two volatile macro-sheet UDFs:

- `FRAMEWORK.DATE.SYSTEM(reference)` returns `1900` or `1904` for the workbook
  containing the reference.
- `FRAMEWORK.DATE.YMD(serial, reference)` resolves that setting, applies it to
  the serial without changing the number, then returns the Gregorian day as
  `YYYY-MM-DD`. Serial days in `[60, 61)` under the 1900 system return `#VALUE!`.

The runner compares 702 observations: two source workbooks, three formula
workbooks, three choices of active workbook, three setting phases, and thirteen
serials. The phases start with 1900/1904, swap both settings while retaining the
same workbook identities, then restore them. Each phase forces a full calculation
rebuild and checks that `Date1904` reads back exactly as assigned before running
comparisons. Both local and cross-workbook references are explicitly qualified;
workbook/sheet names include spaces and apostrophes. The formula workbook and
active workbook can each differ from the source workbook.

Serials include negative fractions, both epochs, values immediately below days
60/61/62, and fractional values on the fictitious leap day. An independent Python
Gregorian calendar oracle uses `floor(serial)`; this test does not introduce or
qualify a date-time rounding policy. The JSONL retains both the requested serial
and the value Excel actually stored, and derives its expected day from the latter.

## Run on Windows

Use desktop Excel and Python with `pywin32` installed. Build/package the fixture
for the installed Excel architecture; for 64-bit Excel, run from the repository
root in PowerShell:

```powershell
cargo run --locked --package cargo-xlfn -- package `
  --manifest-path tests/xlfn-e2e-fixture/Cargo.toml `
  --target x86_64-pc-windows-msvc `
  --out tests/xlfn-e2e-fixture/package --locked

python tests/excel-date-system/run.py `
  --xll <path-to-packaged-FrameworkFixture.xll> `
  --output <new-path-for-date-system-evidence.jsonl>
```

Use `i686-pc-windows-msvc` for 32-bit Excel. The output path must be new: existing
evidence is never overwritten. A `Formula2` assignment or COM failure stops the
run with its stage and traceback retained. Callback errors returned as worksheet
values are retained as failed comparisons; there is no fallback to another
formula API or a default date system.

The provenance event records the exact XLL SHA-256, runner SHA-256, local checkout
revision and `git diff --binary HEAD` hash, untracked file hashes, Python/platform
versions, and timestamps. The Excel event adds version, build, installation path,
window handle, and process ID. Every result includes the formula, source/active/
formula workbook names and settings, observed values, expected values, and result.
`ERROR.TYPE = 3` checks the fictitious-day `#VALUE!` independently of UI language.

When the XLL was built elsewhere, pass its builder's source provenance explicitly:

```powershell
python tests/excel-date-system/run.py `
  --xll <path-to-packaged-FrameworkFixture.xll> `
  --output <new-evidence.jsonl> `
  --source-revision <artifact-build-revision> `
  --source-diff-sha256 <artifact-build-git-diff-sha256>
```

These two options are recorded as user-supplied build provenance. The locally
observed checkout is recorded separately and does not prove what built an XLL.
Retain the XLL and its build manifest alongside the JSONL, including the matching
source revision, working diff, and any untracked source files used for the build.

A completed run exits zero only when all comparisons pass and owned-process
cleanup succeeds. Require 702 `case` records, no failed cases, and a terminal
`summary` with `status = "passed"` on
the candidate artifact before closing the native date-system gate. Missing or
`incomplete` summaries leave the gate open. A `cleanup_failed` summary retains
the independent `date_system_status` and `cleanup_status` so successful date
comparisons do not imply successful cleanup. The separately recorded Excel quit
request and process-exit observation distinguish logical cleanup from process
exit; neither is a physical DLL-unload or memory-leak qualification.

## Local preparation checks

```sh
python3 -m py_compile tests/excel-date-system/run.py
cargo check --manifest-path tests/xlfn-e2e-fixture/Cargo.toml --locked
cargo clippy --manifest-path tests/xlfn-e2e-fixture/Cargo.toml --all-targets --locked
```

Callback-denial and malformed-host-response cases belong to the framework's
mocked callback tests. The native runner exercises workbook attribution and
settings through Excel's actual callback implementation.
