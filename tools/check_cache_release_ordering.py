#!/usr/bin/env python3
"""Require the shared final-pin tail's Loom test to detect missing/late fences.

This is a weak-memory regression gate, not an SMT or machine refinement proof.
Only a compiled test's expected stale-value assertion counts as a rejection.
"""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
PROTOCOL = Path("crates/xlfn/src/cache/pin_transitions.rs")
TEST = "cache::tests::loom_final_pin_fence_acquires_all_holders_before_retirement"
MUTATIONS = {
    "missing final-pin acquire fence": ("                    $fence;\n                    $last", "                    $last"),
    "retirement before final-pin acquire fence": (
        "                    $fence;\n                    $last",
        "                    let outcome = $last;\n                    $fence;\n                    outcome",
    ),
}


def main():
    with tempfile.TemporaryDirectory(prefix="xlfn-cache-release-") as directory:
        tree = Path(directory)
        for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml"):
            if (ROOT / name).exists():
                shutil.copy2(ROOT / name, tree / name)
        for name in ("crates", ".cargo", "tools/windows-bindings", "formal/fixtures"):
            shutil.copytree(ROOT / name, tree / name,
                            ignore=shutil.ignore_patterns("target", "__pycache__"))
        env = os.environ.copy()
        env["CARGO_TARGET_DIR"] = str(ROOT / "target/cache-release-ordering")
        command = ["cargo", "test", "-p", "xlfn", "--features", "cache,handles",
                   "--lib", "--locked", TEST, "--", "--exact", "--nocapture"]
        def run():
            result = subprocess.run(command, cwd=tree, env=env, text=True,
                                    capture_output=True, timeout=300)
            return result.returncode, result.stdout + result.stderr
        code, output = run()
        if code or "test result: ok. 1 passed" not in output:
            raise SystemExit("FAIL: unmodified Loom baseline must run and pass exactly one test\n" + output)
        print("PASS: unmodified shared final-pin Loom baseline", flush=True)
        command.insert(command.index("--locked"), "--offline")
        path = tree / PROTOCOL
        source = path.read_text()
        for name, (before, after) in MUTATIONS.items():
            if source.count(before) != 1:
                raise SystemExit("FAIL: mutation anchor changed: " + name)
            path.write_text(source.replace(before, after))
            code, output = run()
            expected = ("running 1 test", "assertion `left == right` failed",
                        "left: 0", "test result: FAILED. 0 passed; 1 failed", TEST)
            if code == 0 or not all(marker in output for marker in expected):
                raise SystemExit("FAIL: expected stale-value assertion for " + name + "\n" + output)
            print("PASS: Loom rejects " + name, flush=True)


if __name__ == "__main__":
    main()
