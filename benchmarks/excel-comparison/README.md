# Excel comparison benchmark

This suite runs matching worksheet workloads in separate Excel processes with
xlfn or Excel-DNA loaded. It writes one JSON record per case and a paired CSV.
Running it requires **64-bit Excel for Windows with dynamic arrays**, Python
3.11+, `pywin32`, and `psutil`. CI builds and checks the fixtures; it does not
run Excel. The `smoke` profile checks the harness, while `full` supplies the
comparison workloads.

Results depend on the Excel build and machine. Keep generated measurements,
traces, and dumps outside version control. Compare both implementations on the
same computer, Excel build, power mode, calculation settings, and trust settings.
Repeat independent sessions and reverse implementation order when evaluating a
performance change.

## Build and run

The [XLL workflow](../../.github/workflows/excel-comparison-xll.yml) builds x86_64
xlfn and Excel-DNA NativeAOT XLLs for the same commit. Its
`excel-comparison-x86_64` artifact contains a baseline and four registration
variants per implementation, matching baseline PDBs, and a SHA-256 manifest.
A second job downloads and verifies the artifact.

From this directory on the Windows Excel machine:

```powershell
py -m pip install pywin32 psutil
gh auth login
./fetch-ci.ps1 -RunId 123456789
py ./run.py --profile smoke --out ./results-smoke.jsonl
py ./run.py --profile full --out ./results-full.jsonl
py ./summarize.py ./results-full.jsonl
```

Use a completed workflow run ID and check out its source commit before running.
For a PR artifact, use the CI merge commit. `fetch-ci.ps1` refuses an existing
destination to avoid mixing artifacts. Its optional `-Repository OWNER/REPO`
and `-Destination PATH` select another repository or destination; pass the same
destination to `run.py --artifacts`.

The runner verifies all ten XLL digests, PE architecture, artifact provenance,
and checkout commit before opening Excel. `--allow-commit-mismatch` permits a
runner-only change compatible with older XLLs and records both revisions.
Changes to fixture functions require fresh XLLs. Run from an interactive desktop
session with a trusted add-in location and resolve Excel security prompts first.

Each case starts a fresh Excel process. The runner requires successful XLL
loading, validates worksheet outputs, and treats timeout, cleanup failure, or
forced process termination as failure. It retains errors in the JSONL output.
Use a new output path for each run; `--append` explicitly adds to an existing
file. Filter with `--id S01 --id S02`, `--variant 1000`, or
`--implementation xlfn`. Preview workloads on any platform:

```sh
python run.py --profile smoke --plan
```

For local fixture development, install the Rust x86_64 MSVC target, .NET 10 SDK,
and Visual Studio C++ tools required by NativeAOT. Run `./build.ps1`, then pass
`--allow-local-artifacts`. `./build.ps1 -Smoke` builds only the 10-extra-function
registration variant. Local artifacts are recorded as `local-unverified`.

## Workloads

| IDs | Workload | Measurements |
| --- | --- | --- |
| S01–S04 | Identity, argument counts, CPU delay, periodic `#NUM!` | Recalculation distribution and throughput |
| M01–M05 | Numeric input/output/copy, array shapes, mixed cell types | Recalculation per element, allocation observations |
| M06 | Within-xlfn Q versus FP12 numeric returns | Balanced timing samples and full-spill validation |
| T01–T03 | ASCII/Japanese input/output/copy | Recalculation per character |
| P01–P04 | Calculation thread counts, CPU work, shared and contended atomics | Throughput, parallel efficiency, tails |
| A01–A04 | Immediate/delayed async, gated fan-out, simultaneous completions | Submission and completion throughput, arrival tails |
| A05–A06 | Pending result invalidation and repeated argument replacement | Final correctness, drain, stale results |
| R01–R07 | Unique/shared topics, update rates, bursts, churn, sustained updates | Subscription, observed delivery, memory and latency drift |
| C01–C04 | Fresh load, extra registrations, first call, workbook open | Startup and registration times |
| W01–W03 | Mixed and finance-like workbooks | End-to-end completion, CPU and peak RSS |
| L01–L03 | Peak memory, repeated calculations, scalar/async/RTD tails | RSS and private-commit observations, latency drift |
| N01–N05 | Within-xlfn Q versus native numeric, Boolean, integer, string and range inputs | Balanced timing samples and native/Q ratios |

The implementations use identical `BENCH.*` worksheet names and must be loaded
in separate processes. Excel-DNA uses its .NET 10 NativeAOT fixture. Its Task
UDF delivery is RTD-backed, while xlfn uses native async. The comparison includes
Excel's RTD throttle and does not isolate native async framework overhead.

Synchronous workloads use manual calculation. Async, RTD, and mixed workloads
use automatic calculation so RTD completions can reach cells. Gated async cases
arm a file-based control channel once using `Application.Run`; a separate
controller releases work without calling Excel. Full A03/A04 batches fit xlfn's
4096 pending-task limit. A05 native cancellation cannot be established through
COM programmatic recalculation and is reported as unsupported; Excel-DNA's case
measures drain after deliberate release, rather than cancellation of its Task.

Matrix, string, and synchronous timings include COM dispatch, Excel dependency
handling, coercion, spills, and completion queries. They are not isolated Rust
conversion times. Recalculation samples retain Calculate/completion breakdowns;
explicit memory reads occur outside the timer. Async/RTD tails are COM arrival
observations sampled around 10 ms apart. Excel can coalesce RTD updates, so
observed cell changes are a lower bound on publication frequency. The full R07
case runs for 30 minutes.

The numeric input fixture uses explicit `NumericMatrixRef` coercion; strict Q
and native input paths are also compared within xlfn by N01–N05. M06 builds the
same numeric output under Q and FP12. Both comparisons alternate ABBA and BAAB
blocks, validate outputs, retain timing vectors, and report signed changes.
They are within-xlfn controls and have no Excel-DNA performance ratio. For example:

```powershell
py ./run.py --id M06 --implementation xlfn --repeat 10 --out ./results-returns.jsonl
py ./run.py --id N01 --id N04 --implementation xlfn --repeat 10 --out ./results-inputs.jsonl
```

The paired summarizer requires successful results with matching settings,
runner sources, Excel build, and CI artifacts. A ratio above 1 indicates an
xlfn advantage. Raw Rust allocator counts and Excel-DNA managed allocation
counts have different scopes; compare cross-framework timing and process memory
instead. RSS growth alone does not establish a leak. FP12 process-heap buffers
also bypass the Rust allocator counters.

M05 enables allocation tracking after warmup and disables it in `finally`.
Other ordinary workloads run with Rust allocation tracking disabled. The
optional `--repeat-diagnostics` enables it for L02; those instrumented timings
are excluded from comparison ratios. `--shutdown-diagnostics` captures lifecycle
and exit observations, while `--dump-on-failure` captures a surviving process
only after failure. `--full-memory-dumps` adds heap memory to either dump mode.
These troubleshooting modes write local artifacts and may affect timing;
full lifecycle diagnostics are excluded from performance ratios.

## Local checks

From the repository root:

```sh
python -B -m unittest discover -s benchmarks/excel-comparison -p 'test_*.py'
cargo test --manifest-path benchmarks/excel-comparison/xlfn/Cargo.toml --locked --lib
dotnet run --project benchmarks/excel-comparison/control-tests/ControlTests.csproj -- normal
dotnet run --project benchmarks/excel-comparison/control-tests/ControlTests.csproj --no-build -- emergency
```

These checks maintain the runner, output validation, process cleanup, and fixture
control protocols. Native Windows XLL loading and live Excel delivery require
running the benchmarks on Windows with Excel installed.
