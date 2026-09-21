#!/usr/bin/env python3
"""Report installed atomic contract coverage; this is not a correctness gate.

An unproved native initial-value assertion records missing proof evidence, not a
Rust atomic bug. The permission-backed control distinguishes that boundary from
a general verifier failure. No project assumption or external body is introduced.
"""
from pathlib import Path
import json
import subprocess
import tempfile

from check_drain_gate_refinement import is_verification_failure

PROBES = {
    "native_bool_load_call": """
fn probe(value: &std::sync::atomic::AtomicBool) -> bool {
    value.load(std::sync::atomic::Ordering::Acquire)
}
""",
    "native_bool_initial_value": """
fn probe() {
    let value = std::sync::atomic::AtomicBool::new(false);
    let observed = value.load(std::sync::atomic::Ordering::Acquire);
    assert(!observed);
}
""",
    "native_usize_initial_value": """
fn probe() {
    let value = std::sync::atomic::AtomicUsize::new(1);
    let observed = value.load(std::sync::atomic::Ordering::Acquire);
    assert(observed == 1);
}
""",
    "permission_bool_initial_value": """
fn probe() {
    let (value, Tracked(permission)) = vstd::atomic::PAtomicBool::new(false);
    let observed = value.load(Tracked(&permission));
    assert(!observed);
}
""",
}


def main():
    version = subprocess.run(["verus", "--version"], text=True, capture_output=True, check=True, timeout=30)
    report = {"verus": version.stdout.strip(), "probes": {}}
    with tempfile.TemporaryDirectory(prefix="xlfn-atomic-contracts-") as directory:
        for name, source in PROBES.items():
            path = Path(directory) / (name + ".rs")
            path.write_text("use vstd::prelude::*;\nverus! {\n" + source + "\n}\n")
            result = subprocess.run(["verus", "--crate-type=lib", str(path)],
                                    text=True, capture_output=True, timeout=120)
            output = result.stdout + result.stderr
            if result.returncode == 0 and "1 verified, 0 errors" in output:
                status = "proved"
            elif is_verification_failure(result.returncode, output):
                status = "unproved"
            else:
                raise SystemExit("Atomic contract probe failed to run: " + name + "\n" + output)
            report["probes"][name] = status
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
