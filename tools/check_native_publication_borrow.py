#!/usr/bin/env python3
"""Reject lifetime and exclusivity violations of the native publication guard (not an SMT test)."""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
BINDING = Path("crates/xlfn/src/handle/binding.rs")


def main():
    with tempfile.TemporaryDirectory(prefix="xlfn-native-publication-") as directory:
        tree = Path(directory)
        for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml"):
            if (ROOT / name).exists():
                shutil.copy2(ROOT / name, tree / name)
        for name in ("crates", ".cargo", "tools/windows-bindings"):
            shutil.copytree(ROOT / name, tree / name,
                            ignore=shutil.ignore_patterns("target", "__pycache__"))
        env = os.environ.copy()
        env["CARGO_TARGET_DIR"] = str(ROOT / "target/native-publication-borrow")
        command = ["cargo", "check", "-p", "xlfn", "--no-default-features",
                   "--features", "handles", "--lib", "--locked"]
        def check():
            return subprocess.run(command, cwd=tree, env=env, text=True,
                                  capture_output=True, timeout=300)
        baseline = check()
        if baseline.returncode:
            raise SystemExit("FAIL: native baseline does not compile\n" + baseline.stdout + baseline.stderr)
        # The baseline fetches locked dependencies on fresh CI runners.
        # Mutations must use the same resolved dependencies without network.
        command.append("--offline")
        path = tree / BINDING
        source = path.read_text()
        before = """        self.table
            .publication_writer(&mut state)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop())
            .insert(self.id, pointer);"""
        after = """        let mut writer = self.table
            .publication_writer(&mut state)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        drop(state);
        writer.insert(self.id, pointer);"""
        if source.count(before) != 1:
            raise SystemExit("FAIL: native publication mutation anchor changed")
        duplicate = after.replace(
            "        drop(state);",
            "        let _other_writer = self.table.publication_writer(&mut state);")
        cases = [
            ("guard release before publication", after,
             ("error[E0505]: cannot move out of `state` because it is borrowed",
              "borrow later used here")),
            ("simultaneous writer capabilities", duplicate,
             ("error[E0499]: cannot borrow `state` as mutable more than once at a time",
              "first borrow later used here")),
        ]
        for name, mutation, expected in cases:
            path.write_text(source.replace(before, mutation))
            result = check()
            output = result.stdout + result.stderr
            if result.returncode == 0 or not all(marker in output for marker in expected):
                raise SystemExit("FAIL: expected native guard lifetime error for " + name + "\n" + output)
            print("PASS: native borrow checker rejects " + name, flush=True)

        path.write_text(source)
        barrier_path = tree / "crates/xlfn-kernel/src/rotating_read_domain.rs"
        barrier_source = barrier_path.read_text()
        before = "protocol::publish_release!(publish(&barrier), drop(barrier));"
        after = "protocol::publish_release!(drop(barrier), publish(&barrier));"
        if barrier_source.count(before) != 1:
            raise SystemExit("FAIL: native barrier mutation anchor changed")
        barrier_path.write_text(barrier_source.replace(before, after))
        result = check()
        output = result.stdout + result.stderr
        if result.returncode == 0 or "error[E0382]: borrow of moved value: `barrier`" not in output:
            raise SystemExit("FAIL: expected native early barrier release error\n" + output)
        print("PASS: native borrow checker rejects barrier release before generation publication", flush=True)


if __name__ == "__main__":
    main()
