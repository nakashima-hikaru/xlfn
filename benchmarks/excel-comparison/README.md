# Excel comparison benchmark

This suite runs the same worksheet workloads in separate Excel processes with
either xlfn or Excel-DNA loaded. It produces one JSON record per case and a
paired CSV. It needs **64-bit Excel for Windows (Microsoft 365 with dynamic
arrays)** to run. The smoke profile checks the harness; use `full` for a
comparison.

The separate [lifecycle terminal-status record](lifecycle-terminal.md) covers
callback suppression during reload and rollback. The
[async event qualification gate](event-qualification/README.md) records the
remaining live-Excel check for nil-procedure event removal. Worksheet comparison
results do not establish either native lifecycle behavior or event removal.

## Build in CI, run on the Windows Excel machine

The [Excel comparison XLLs workflow](../../.github/workflows/excel-comparison-xll.yml)
builds **x86_64** xlfn and Excel-DNA **NativeAOT** XLLs for the same commit. It
uploads `excel-comparison-x86_64`, containing both `benchmark.xll` files,
four registration variants per implementation, and a manifest with SHA-256
digests. A second CI job downloads and verifies that artifact. CI does not
run Excel.

On the Excel machine, install Python 3.11+, `pywin32`,
`psutil`, and GitHub CLI (`gh`). Use the same Excel build, computer, power
mode, and trust settings for both add-ins. Run from an ordinary interactive
desktop session with a trusted add-in location; Excel COM automation is not
suitable for a Windows service. Check out the revision used by the CI run,
then download its artifact by **run ID**:

```powershell
py -m pip install pywin32 psutil
gh auth login
./fetch-ci.ps1 -RunId 123456789
py ./run.py --profile smoke --out ./results-smoke.jsonl
py ./run.py --profile full --out ./results-full.jsonl
py ./summarize.py ./results-full.jsonl
```

Replace `123456789` with a completed workflow run ID. `fetch-ci.ps1` rejects
an existing destination so artifacts from different runs cannot be mixed;
`-Repository OWNER/REPO` and `-Destination PATH` are available if needed. If
you choose another destination, pass the same path to `run.py --artifacts`.
The runner verifies the CI manifest (including Excel-DNA's NativeAOT build mode), all ten XLL digests, the x86_64 PE
machine type, and the checkout commit before opening Excel. For a PR artifact,
check out the CI run's merge commit; `--allow-commit-mismatch` is available
only for runner-only changes that remain compatible with the artifact's
worksheet functions. Changes to either XLL require new artifacts. Records include
the CI commit and run ID, and pairing requires the same run. Each case starts
a fresh Excel process;
the parent kills a timed-out process and records a failed case. `--id S01
--id S02`, `--variant 1000`, and `--implementation xlfn` narrow a run.
`py ./run.py --plan` lists cases without opening Excel and also works on a
Mac. Choose a new `--out` path for each run; use `--append` only when
intentionally adding cases to an existing JSONL file.

The runner loads the selected XLL with `Application.RegisterXLL` and stops
if Excel reports a load failure. Formula checks report Excel errors such as
`#NAME?` explicitly. Scalar checks compare every cell with its expected value;
S04 checks both the location and type of each intended `#NUM!` result.
Scalar results from runner revisions through `2c06039` must be rerun: those
revisions could mistakenly accept Excel error codes as numeric results outside S04.

For development without CI, install the Rust x86_64 MSVC target, .NET 10
SDK, and the Visual Studio C++ toolchain needed by NativeAOT; run
`./build.ps1`, then pass `--allow-local-artifacts` to `run.py`.
`build.ps1 -Smoke` creates only the 10-extra registration variant. Local
artifacts are labeled `local-unverified` and do not enter the normal CI path.

The two packed XLLs use identical `BENCH.*` worksheet names and are **never
loaded together**. The Excel-DNA fixture uses the 1.10.0-preview5 NativeAOT
package on .NET 10; its
Task UDF uses Excel-DNA's built-in **RTD-backed** async registration, and its streaming UDF
uses `ExcelAsyncUtil.Observe`. The xlfn fixture uses xlfn's async and RTD
paths. The worksheet function bodies do only the stated work. C02 generates
the same number of extra one-argument identity functions for each add-in;
the ordinary fixture functions remain registered too. Async measurements compare
xlfn native async delivery with Excel-DNA RTD-backed Task delivery, including
Excel's RTD throttle; they do not isolate native async framework overhead.

The synchronous paths have different representation costs. xlfn uses
XLOPER12 arguments and returns with strict cell validation and an
`xlAutoFree12` cleanup obligation. Excel-DNA uses typed numeric and string
marshaling; its numeric-array path can use dense FP12 storage. Rust strings
also require UTF-16 to UTF-8 conversion on input and conversion back on output.
T01 counts Unicode scalar values in Rust but reads the UTF-16 length in C#;
the current ASCII and Japanese BMP inputs have the same expected count.
These cases compare the exposed APIs, including those costs. They do not
isolate equivalent host ABI paths or the function body alone.

## Workloads and measurement

| IDs | What the harness does | Main observed value |
| --- | --- | --- |
| S01–S04 | Identity, 2/4/8 args, timed busy loop, periodic `#NUM!` | Dirty-range recalculation distribution and throughput; S03 has a 0-µs baseline |
| M01–M05 | Numeric input/output/copy, equal-element tall/wide/square, mixed Excel cell types | Recalculation time per element; M05 also records add-in allocation counters |
| T01–T03 | ASCII/Japanese input/output/copy, 8–4096 characters | Recalculation time per character |
| P01–P04 | 1/2/4/8/16 Excel calculation threads; CPU/light/heavy/contended atomic UDF | Throughput, speedup versus one thread, parallel efficiency, batch tails |
| A01–A04 | Immediate/delayed async; gated 100/1k/4096 fan-out and 4096 simultaneous completions | Submission and completion throughput, COM-observed cell-arrival tails |
| A05–A06 | Dirty/clear/close pending calls; repeatedly replace arguments before prior completion | Task cleanup, final-result correctness, stale result count |
| R01–R03 | Unique/shared/grouped topics, including 100k unique | Subscribe time, incremental RSS, pulse update time |
| R04–R07 | 1–2000 requested updates/topic/s, burst, churn, 30-minute run | Source emissions, observed cell updates, tail, RSS/latency drift |
| C01–C04 | Fresh Excel/add-in load, extra registrations, first call, warm workbook open | Startup/registration/first-call/open-to-complete time |
| W01–W03 | Mixed/finance-like/10k–100k formula books | End-to-end settle, Excel CPU, peak RSS |
| L01–L03 | Representative peak memory, 100/1000 recalculations, scalar/async/RTD tails | Peak RSS per cell, latency/RSS drift, p50/p95/p99/max |

Async, RTD, and mixed workloads use automatic calculation for both add-ins so
RTD completions can update cells. Synchronous workloads use manual calculation.
The result records `calculation_mode` and, for async workloads, `async_delivery`.
A01/A02 timing starts before formula entry because automatic calculation may
begin while formulas are being submitted. No extra recalculation is requested
while async results are pending, including the A03/A04 gated calls. Those cases
release the gate through `Application.Evaluate("BENCH.ASYNC.RELEASE(1)")`, without
writing a control formula to the worksheet or requesting another calculation.
The full profile uses A03 variants `100`, `1000`, and `4096`, and A04 variant
`burst-4096`. Both implementations receive the same counts, which fit xlfn's
4096 simultaneously pending native async tasks. Earlier A03/`10000` and
A04/`burst` full-profile results used 10,000 gated tasks; that count exceeds
xlfn's capacity and cannot reach the all-active condition. Keep those historical
rows separate from the new variants. This measures a single simultaneous batch;
it does not release tasks in waves. The smoke profile still uses 20 tasks.

Synchronous timing starts after formula creation and `Range.Dirty()` and ends
when Excel reports calculation done. The explicit RSS read after each
recalculation happens after the timer stops; the background RSS sampler still
runs throughout the case. Older runners also included that explicit
process-information read in each recalculation sample. Matrix output is checked at its bottom
right spill cell. Async timing checks the **actual cell values**, not just
Excel's calculation state. A03/A04 wait until the requested number of calls
is active before releasing a shared gate. RTD timing waits for subscription
count, then checks cell values after a pulse. RTD update-rate results include
both the source's actual emission rate and the values observed in a cell; the
latter is a lower bound because Excel can coalesce updates. The runner sets
`RTD.ThrottleInterval` to 100 ms by default and restores the prior value when
it closes the Excel process. Async and RTD tails are sampled through COM at
about 10 ms intervals, so they are **arrival observations**, not precise
in-process callback latency. `R07` is 30 minutes in the full profile.

A02's 100-µs case uses a timed CPU wait in both fixtures, since ordinary
async timers need not resolve 100 µs. Delays of 1 ms and above use async
timers. Treat the 100-µs point as a different delay mechanism.

`results-full.jsonl` is the evidence record: case parameters, Excel
version/build, thread setting, RTD throttle, add-in SHA-256, individual
metrics, errors, and peak Excel RSS. `summarize.py` compares only successful
records with matching settings and reports an xlfn advantage ratio greater
than 1 when xlfn wins. It also derives P-series speedup and efficiency. Raw
M05 allocation counts have different scope: xlfn counts Rust allocator requests
in the add-in and Excel-DNA counts managed allocations. Use timing and
RSS for cross-framework comparison; inspect raw allocations only within an
implementation. `R07` RSS growth is observational and affected by Excel's
own caching and garbage collection.

Rust allocation counting is disabled by default. M05 enables it after warmup
with `BENCH.ALLOC.TRACK(TRUE)` and disables it in a `finally` block after the
counter reads and recalculations. These control calls are outside the timed
recalculations. The matching Excel-DNA control is a no-op because its CLR
allocation counter is already available. M05 timing includes Rust's counting
overhead; other workloads only check the disabled flag when allocating.
Earlier fixtures incremented a shared atomic counter on every Rust allocation
and reallocation throughout every workload. Those historical results include
that instrumentation cost and should be rerun with newly built XLLs before
attributing the differences to the frameworks.

The Rust RTD fixture starts its source worker at the first subscription and
waits when there is no periodic or pulse work. Earlier fixtures started a
worker during add-in open and woke it every millisecond even in synchronous
workloads. That background activity is another reason to rerun the historical
CSV with newly built XLLs; its effect on the recorded timings is unmeasured.

C01 starts a new Excel process, but the operating system may still cache add-in
files between cases. For storage-cold startup, reboot or flush the machine's
file cache under a controlled protocol and record that condition separately.

The Windows run must be checked manually for Excel security prompts, XLL
load errors, dynamic-array spill errors, and add-in shutdown behavior before
interpreting performance. This repository's Mac checks cannot establish
Windows compilation or live Excel correctness.

Relevant API references: [Excel-DNA NativeAOT support](https://excel-dna.net/docs/guides-basic/dotnet-native-aot-support/),
[Excel-DNA extended registration](https://excel-dna.net/docs/guides-basic/extended-registration/),
[Excel-DNA packed add-in build properties](https://excel-dna.net/docs/guides-basic/sdk-style-project-properties/),
[Excel `Range.Dirty`](https://learn.microsoft.com/en-us/office/vba/api/excel.range.dirty),
and [Excel RTD throttle](https://learn.microsoft.com/en-us/office/vba/api/excel.rtd.throttleinterval).
