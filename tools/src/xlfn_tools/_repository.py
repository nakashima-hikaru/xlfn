"""Locate checkout data independently of the process working directory."""

from pathlib import Path


def repository_root() -> Path:
    # Editable installs bind tools to their own checkout. A built wheel can
    # instead discover the checkout containing its environment or the cwd.
    for origin in (Path(__file__).resolve(), Path.cwd()):
        for candidate in (origin, *origin.parents):
            if (
                (candidate / "Cargo.toml").is_file()
                and (candidate / "tools/pyproject.toml").is_file()
                and (candidate / "crates/xlfn-kernel/Cargo.toml").is_file()
            ):
                return candidate
    raise RuntimeError("xlfn tools require an xlfn checkout; run from its repository directory")


ROOT = repository_root()
