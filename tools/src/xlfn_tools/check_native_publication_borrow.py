#!/usr/bin/env python3
"""Reject native publication-guard and Cache ownership violations (not an SMT test)."""

import os
import shutil
import subprocess
import tempfile
from pathlib import Path

from ._repository import ROOT

BINDING = Path("crates/xlfn/src/handle/binding.rs")


def main():
    with tempfile.TemporaryDirectory(prefix="xlfn-native-publication-") as directory:
        tree = Path(directory)
        for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml"):
            if (ROOT / name).exists():
                shutil.copy2(ROOT / name, tree / name)
        for name in ("crates", ".cargo", "tools/windows-bindings"):
            shutil.copytree(
                ROOT / name, tree / name, ignore=shutil.ignore_patterns("target", "__pycache__")
            )
        env = os.environ.copy()
        env["CARGO_TARGET_DIR"] = str(ROOT / "target/native-publication-borrow")
        command = [
            "cargo",
            "check",
            "-p",
            "xlfn",
            "--no-default-features",
            "--features",
            "handles",
            "--lib",
            "--locked",
        ]

        def check():
            return subprocess.run(
                command, cwd=tree, env=env, text=True, capture_output=True, timeout=300
            )

        baseline = check()
        if baseline.returncode:
            raise SystemExit(
                "FAIL: native baseline does not compile\n" + baseline.stdout + baseline.stderr
            )
        # The baseline fetches locked dependencies on fresh CI runners.
        # Mutations must use the same resolved dependencies without network.
        command.append("--offline")
        path = tree / BINDING
        source = path.read_text()
        # BindingWriter now owns its issuing table's guard. Exercise the actual
        # publication API through an exclusive borrow of that writer, including
        # release of the owned guard (not merely an &mut reference).
        probe = """
#[allow(dead_code, reason = "Compile-only publication capability probe")]
fn publication_guard_probe(table: &BindingTable, index: usize) {
    let mut writer = table.writer();
    let publication = &mut writer;
    publication.publish_slot(index);
    drop(writer.state);
}
"""
        path.write_text(source + probe)
        accepted = check()
        if accepted.returncode:
            raise SystemExit(
                "FAIL: valid native publication caller does not compile\n"
                + accepted.stdout
                + accepted.stderr
            )
        after = probe.replace(
            "    publication.publish_slot(index);\n    drop(writer.state);",
            "    drop(writer.state);\n    publication.publish_slot(index);",
        )
        duplicate = probe.replace(
            "    publication.publish_slot(index);",
            "    let other_publication = &mut writer;\n"
            "    publication.publish_slot(index);\n"
            "    other_publication.publish_slot(index);",
        )
        cases = [
            (
                "guard release before publication",
                after,
                (
                    "error[E0505]: cannot move out of `writer.state` because it is borrowed",
                    "borrow later used here",
                ),
            ),
            (
                "simultaneous writer capabilities",
                duplicate,
                (
                    "error[E0499]: cannot borrow `writer` as mutable more than once at a time",
                    "first borrow later used here",
                ),
            ),
        ]
        for name, mutation, expected in cases:
            path.write_text(source + mutation)
            result = check()
            output = result.stdout + result.stderr
            if result.returncode == 0 or not all(marker in output for marker in expected):
                raise SystemExit(
                    "FAIL: expected native guard lifetime error for " + name + "\n" + output
                )
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
        print(
            "PASS: native borrow checker rejects barrier release before generation publication",
            flush=True,
        )

        barrier_path.write_text(barrier_source)
        if "--offline" in command:
            command.remove("--offline")
        command[command.index("--features") + 1] = "handles,cache"
        baseline = check()
        if baseline.returncode:
            raise SystemExit(
                "FAIL: native Cache baseline does not compile\n" + baseline.stdout + baseline.stderr
            )
        command.append("--offline")
        cache_path = tree / "crates/xlfn/src/cache.rs"
        cache_source = cache_path.read_text()
        anchor = "impl<V> std::ops::Deref for CacheLease<'_, V> {"
        probe = """
fn borrowed_cache_lease_probe<V>(lease: CacheLease<'_, V>) {
    let value = &*lease;
    drop(lease);
    std::hint::black_box(value);
}
"""
        if cache_source.count(anchor) != 1:
            raise SystemExit("FAIL: native Cache lease lifetime anchor changed")
        cache_path.write_text(cache_source.replace(anchor, probe + anchor))
        rejected = check()
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0505]: cannot move out of `lease` because it is borrowed",
            "borrow later used here",
            "cache.rs",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected native live-Cache-lease-reference error\n" + output)
        print(
            "PASS: native borrow checker rejects dropping a Cache lease while its value reference is live",
            flush=True,
        )

        cache_path.write_text(cache_source)
        # Raw recovery is private to this child module; a probe in cache.rs
        # would test privacy rather than consumption of the retirement owner.
        reclaim_path = tree / "crates/xlfn/src/cache/reclamation.rs"
        reclaim_source = reclaim_path.read_text()
        anchor = "unsafe fn reclaim_cache_node<V>(entry: ReclaimEntry<V>) {"
        probe = """
unsafe fn duplicate_cache_reclamation_probe<V>(entry: ReclaimEntry<V>) {
    unsafe { reclaim_cache_node(entry); }
    unsafe { reclaim_cache_node(entry); }
}
"""
        if reclaim_source.count(anchor) != 1:
            raise SystemExit("FAIL: native Cache reclamation ownership anchor changed")
        reclaim_path.write_text(reclaim_source.replace(anchor, probe + anchor))
        rejected = check()
        output = rejected.stdout + rejected.stderr
        required = ("error[E0382]: use of moved value: `entry`", "reclamation.rs")
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit(
                "FAIL: expected native duplicate-reclamation ownership error\n" + output
            )
        print(
            "PASS: native borrow checker rejects reclaiming the same retirement entry twice",
            flush=True,
        )

        reclaim_path.write_text(reclaim_source)
        probe = """
fn release_cache_domain_before_batch_probe() {
    let domain = CacheLookupDomain::<u8>::new();
    let batch = domain.quiesce_and_drain();
    drop(domain);
    reclaim_cache_entries(batch);
}
"""
        reclaim_path.write_text(reclaim_source.replace(anchor, probe + anchor))
        rejected = check()
        output = rejected.stdout + rejected.stderr
        required = (
            "error[E0505]: cannot move out of `domain` because it is borrowed",
            "borrow later used here",
            "reclamation.rs",
        )
        if rejected.returncode == 0 or not all(marker in output for marker in required):
            raise SystemExit("FAIL: expected native Cache domain-before-batch error\n" + output)
        print(
            "PASS: native borrow checker retains the Cache domain through batch reclamation",
            flush=True,
        )


if __name__ == "__main__":
    main()
