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
