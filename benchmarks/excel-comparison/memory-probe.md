# Repeated-calculation memory attribution

`memory_probe.py` captures allocation stacks for the L02-style workload using
Windows Performance Recorder (WPR). It runs fresh Excel-only, xlfn and Excel-DNA
processes, each with 1,000 cells, one calculation thread, one `CalculateFull`
warmup and 1,000 measured recalculations. The Excel-only control uses `=ROW()`;
the add-in cases use `=BENCH.ID(row)`. The control has the same COM/worksheet
operations, but is not a matched CPU-workload or performance comparison.

This experiment follows the observed memory growth with allocation counters
both enabled and disabled. It captures native heap snapshots and virtual-memory
allocation events; Rust requested-byte totals alone cannot attribute that
process growth. Heap snapshots cover allocations made after PID tracking was
enabled, before the test XLL is loaded. They do not reconstruct startup
allocations made before that point.

## Prerequisites and matching artifacts

Use an interactive Windows x64 desktop with 64-bit Excel and 64-bit Python
3.11 or later. Open PowerShell **as administrator** for WPR. Install `pywin32`
and `psutil` in that Python environment. `wpr.exe` is normally available in
Windows; [Windows Performance Toolkit](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/)
provides WPR and Windows Performance Analyzer (WPA) if needed.

The new CI artifact includes the optimized baseline XLLs and matching PDB
symbol files under `symbols/`. The manifest hashes both, and binds each symbol
bundle to its baseline XLL hash. The memory probe requires these symbols and
an exact checkout/artifact commit match. Artifacts from run `40190045107`
predate the symbol bundle and cannot be used for this probe.

After committing/pushing this change and completion of its **Excel comparison
XLLs (x86_64)** workflow, run the following from the Windows repository root.
The workflow lookup selects a successful run for the exact checked-out commit;
it does not select an unrelated newer build.

```powershell
git pull --ff-only
if ($LASTEXITCODE -ne 0) { throw 'git pull failed' }
$probeCommit = (git rev-parse HEAD).Trim()
$probeRun = gh run list --repo nakashima-hikaru/xlfn --workflow excel-comparison-xll.yml --commit $probeCommit --status success --limit 1 --json databaseId --jq '.[0].databaseId'
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($probeRun) -or $probeRun -eq 'null') {
    throw 'No successful XLL workflow for this commit yet; wait for its CI run'
}
Set-Location .\benchmarks\excel-comparison
$probeArtifacts = "artifacts-memory-$($probeRun.Trim())"
if (Test-Path -LiteralPath $probeArtifacts) { throw 'Choose a new artifact destination' }
gh run download $probeRun --repo nakashima-hikaru/xlfn --name excel-comparison-x86_64 --dir $probeArtifacts
if ($LASTEXITCODE -ne 0) { throw 'Artifact download failed' }
python .\artifact_manifest.py verify --root $probeArtifacts --commit $probeCommit --require-symbols
if ($LASTEXITCODE -ne 0) { throw 'Artifact or symbol verification failed' }
python .\memory_probe.py --artifacts $probeArtifacts --out memory-probe-01
```

The output directory must not exist. The default command records all three
implementations in order, in separate Excel processes. To select one case,
add `--implementation xlfn`, `--implementation excel_dna` or
`--implementation excel`. A retry needs a fresh `--out` directory. To collect
only virtual-memory events, use `--trace virtual`; `--trace heap` selects only
heap snapshots. These alternate modes produce different evidence and are
identified in `report.json`.

## What is captured

A child process owns Excel COM. The parent validates the new Excel PID and
native creation time, holds its process handle, enables heap snapshots for
that PID, and starts a uniquely named WPR recording before allowing XLL load.
It uses the documented
[PID-scoped heap snapshot mechanism](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/record-heap-snapshot)
and [WPR command options](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/wpr-command-line-options).
It does not change the image-wide IFEO settings for `EXCEL.EXE`, or stop/cancel
the default WPR recording. A returned pre-existing Excel process is rejected
before changing its settings or loading an XLL.

The worker pauses at six barriers while the parent records markers and heap
snapshots:

| Label | State |
| --- | --- |
| `post_warmup` | Formula values validated after `CalculateFull` warmup |
| `after_250` | 250 measured recalculations completed |
| `after_500` | 500 measured recalculations completed |
| `after_1000` | 1,000 measured recalculations completed |
| `workbook_closed` | Worksheet/range references released and test workbook closed |
| `after_idle` | Five seconds after closing the test workbook |

At the final barrier, the parent disables PID snapshot instrumentation and
saves the trace while Excel is still alive. Then the worker restores its
settings, closes its bootstrap workbook, and calls Quit. The XLL remains loaded
through the workbook-close/idle snapshots. These phases do not test add-in
close/reopen or physical DLL unload.

The parent bounds the worker runtime (default 300 seconds per case), WPR
commands, and recording storage (default 1,024 MiB per case, checked between
commands). A snapshot/stop may temporarily exceed the storage threshold before
returning. Errors preserve partial evidence and attempt to save/cancel only the
owned recording. Forced cleanup is restricted to the process whose native
handle and identity were validated. Every failure and forced termination is
recorded. If a terminal is forcibly closed or the machine is interrupted,
`trace.json` records the unique instance name for targeted recovery:

```powershell
# Replace the instance with the exact value in this run's trace.json.
wpr -stop recovered-memory.etl -instancename xlfn-memory-INSTANCE_FROM_TRACE_JSON
```

Do not omit `-instancename`. PID snapshot settings expire when that process
exits. Normal cleanup explicitly disables them before saving the trace.

## Outputs and interpretation

`report.json` contains the CI manifest, XLL/PDB hashes, runner source hashes,
Excel identity, phase clocks, memory samples, validation results, cleanup and
WPR command records. Each implementation directory contains `memory.etl`,
`trace.json`, `worker.json`, `worker.log` and control files. Preserve the complete
output directory and matching artifact's `symbols/` tree.

`capture_status: complete` means the requested commands, phases and worksheet
validation completed. **It does not mean the allocation owner was identified.**
The report retains `attribution_status: not_analyzed` and
`trace_contents_verified: false` until the ETL is inspected. Check event loss,
actual snapshot presence and symbol resolution before drawing conclusions.
The raw WPR collector status is in `trace.json`; it is not parsed using
language-dependent success strings.

Open each ETL in WPA and filter to the process PID/creation time in its report.
Add the matching `symbols/xlfn` and `symbols/excel_dna` directories (including
any subdirectories containing PDBs) to WPA's symbol search paths. Microsoft
public symbols can resolve Windows and available Office frames. Use
**Trace → Load Symbols**; unresolved private Excel frames may remain as module
names and offsets. The ETL's image mapping and PDB identity determine whether
a file resolves; a filename alone is not a symbol match.

In the Memory **Heap Snapshots** view, locate the six snapshots and group live
allocation size/count by stack. Compare `post_warmup` with `after_1000`, then
with `workbook_closed` and `after_idle`. Inspect the frames above heap/runtime
allocation wrappers to distinguish xlfn requests, Excel/COM code and tracing
bookkeeping. Preserve full stacks, snapshot labels, count and size columns in
CSV exports or a saved WPA analysis profile; do not report cumulative allocated
bytes as retained bytes.

For growth outside these heaps, inspect the Memory **VirtualAlloc Commit
LifeTimes** view and its commit stacks over the marker intervals. Heap backing
reservations and individual heap allocations overlap; **do not add their sizes**.
RSS, private commit, outstanding heap blocks and virtual address reservations
measure different things. Tracing itself changes timing and memory, including
the heap stack database. The instrumented process totals are not comparable
performance measurements to ordinary L02 results.

Compare the Excel-only control and both add-ins before assigning a cause.
Memory released at workbook close, persistent allocator backing storage, and
live blocks with increasing counts lead to different follow-up investigations.
A live block at one snapshot is not by itself a leak. Include unresolved
symbols and dropped-event limitations in any attribution report.

## Validation boundary

Unit tests cover worksheet validation, snapshot/protocol ordering, partial
captures, PID reuse/pre-existing-process refusal, PDB provenance, command
failures and cleanup. The XLL workflow additionally runs an opt-in native WPR
smoke test against a disposable Python process on Windows x64 and saves its
trace as `memory-probe-smoke-x86_64`. This checks actual WPR command execution
without Excel; it does not establish Excel memory attribution or prove that
all ETL stacks are complete. Native Excel capture and WPA inspection remain
required on the target machine.
