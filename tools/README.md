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

## Feature shard plans

The Windows library feature checker and production tests use the same
manifest-derived inventory as the former cargo-hack powersets: 288 distinct
check cases and 96 test cases, including the empty selection. The runner
deduplicates transitive local feature selections (for example,
`async-builtin` already enables `async`), and excludes `default` in both modes
and `bench-internals`/`refinement` in test mode. Each test case still runs the
library suite with one test thread.

To inspect a plan without building:

```sh
uv run --project tools --locked python -B -m xlfn_tools.feature_matrix check --shard 1/4 --list
uv run --project tools --locked python -B -m xlfn_tools.feature_matrix test --shard 1/8 --list
```

With no shard option, the command runs the complete inventory. CI runs four
`check` shards (72 cases per job) and eight `test` shards (12 cases per job).
For `check`, the deterministic cost proxy starts at 10 per case, adds 3 for the
RTD transport shared by `rtd` and `handles`, adds 2 each for `handles`,
`cache`, and `async`, adds 1 for each other enabled feature (including
`async-builtin` and `bench-internals`), and assigns the heaviest cases to the
lightest non-full shard first (total proxy weight 1320 per check shard). For
`test`, scheduling balances shards using historical Windows durations from
`tools/feature-test-costs.json` (834–835 s per shard, falling back to `4 * weight`
for new combinations). Each case prints its elapsed time so Windows CI can be
used to validate or tune the cost model.
