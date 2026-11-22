# Repository Python tools

The verification tools use an installed `xlfn_tools` package in
`src/xlfn_tools`, with unit tests in `tests`. The uv project requires Python
3.15; uv selects it from `.python-version` and can download it automatically.
Install uv 0.13.4 or newer before using the commands below. A separate system
Python, global Ruff, or global ty is not required.

From the repository root:

```sh
uv sync --project tools --locked
just python-check
```

`python-check` runs Ruff lint and format checks, ty, every `tools/tests/test_*.py`
unit test. The same recipe runs in CI. Native Rust/Verus checks remain separate
recipes.

`uv run` installs the package before executing a module. Run an individual tool
from the repository root:

```sh
uv run --project tools --locked python -B -m xlfn_tools.check_panic_boundaries
uv run --project tools --locked python -B -m xlfn_tools.check_typestate
```

From another directory, select this checkout's `tools` project by its absolute
path. Editable installs find the checkout from the package location, so checks
do not depend on the working directory or a manually modified `PYTHONPATH`.
Allowlist files and typestate fixtures remain under `tools` as checkout data.

The equivalent individual quality checks are:

```sh
uv run --project tools --locked ruff check tools
uv run --project tools --locked ruff format --check tools
uv run --project tools --locked ty check --project tools --error-on-warning
uv run --project tools --locked python -B -m unittest discover -s tools/tests -p 'test_*.py'
```

Use `ruff check --fix tools` and `ruff format tools` through the same `uv run`
prefix to apply lint fixes and formatting. The ty `--project tools` argument
selects the type-checking scope; uv's own `--project` selects the environment
and does not change the working directory.

`pyproject.toml` declares the development dependencies and Python version range;
`uv.lock` records exact versions and artifact hashes for supported platforms.
Keep the lockfile in version control. To intentionally update the quality tools:

```sh
uv lock --project tools --upgrade-package ruff --upgrade-package ty
uv sync --project tools --locked
just python-check
```

The virtual environment, caches, and generated verification results are local
artifacts and must stay out of version control. Scripts continue to launch
Python subprocesses through `sys.executable` so child checks use the same
managed interpreter.

## Feature and Verus shard plans

The Windows library feature checker and production tests use the same
manifest-derived inventory as the former cargo-hack powersets: 288 distinct
check cases and 96 test cases, including the empty selection. The runner
deduplicates transitive local feature selections (for example,
`async-builtin` already enables `async`), and excludes `default` in both modes
and `bench-internals`/`refinement` in test mode. Each test case still runs the
library suite with one test thread.

To inspect a plan without building:

```sh
uv run --project tools --locked python -B -m xlfn_tools.feature_matrix check --shard 1/8 --list
uv run --project tools --locked python -B -m xlfn_tools.feature_matrix test --shard 1/8 --list
```

With no shard option, the command runs the complete inventory. CI runs eight
shards for each mode: 36 check or 12 test cases per job. The deterministic
cost proxy starts at 10 per case, adds 3 for the RTD transport shared by
`rtd` and `handles`, adds 2 each for `handles`, `cache`, and `async`, and
adds 1 for each other enabled feature. The heaviest cases go to the lightest
non-full shard first. Total proxy weights are 659–661 for check jobs and
202–204 for test jobs. These are not measured Windows runtimes. Each case
prints its elapsed time so Windows CI can be used to validate or tune the proxy.

Verus shards assign each exact input snapshot and all of its mutations to one
runner. A runner proves the unmodified snapshot before starting any local
mutation. Shard assignment uses the number of verifier calls (one baseline plus
its mutations) as a proxy, so indivisible large suites can still dominate the
largest job. Across four jobs the current plan executes eight baselines rather
than thirty-two, and retains all 423 mutations. The required Verus check
reconstructs the current plan and requires every expected baseline and mutation
in its designated report, with matching source digests and Verus versions.
