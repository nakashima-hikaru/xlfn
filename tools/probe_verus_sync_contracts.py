#!/usr/bin/env python3
"""Report native std Mutex/Condvar support without claiming a refinement proof.

The production DrainGate uses parking_lot, not these std types. Rejection of the
std types establishes only that their direct Verus path is unavailable in the
pinned verifier; it does not establish anything about parking_lot semantics.
Future Verus support is reported as proved or unproved, not treated as failure.
"""
from pathlib import Path
import json
import subprocess
import tempfile

from check_drain_gate_refinement import is_verification_failure

PROBES = {
    "std_mutex_value": (
        """fn probe() {
    let lock = std::sync::Mutex::new(7u8);
    let guard = lock.lock().unwrap();
    assert(*guard == 7);
}""",
        "`std::sync::poison::mutex::Mutex` is not supported",
    ),
    "std_condvar_new": (
        """fn probe() {
    let _condition = std::sync::Condvar::new();
}""",
        "`std::sync::poison::condvar::Condvar` is not supported",
    ),
}


def classify(code, output, unsupported):
    if code == 0 and "verification results:: 1 verified, 0 errors" in output:
        return "proved"
    if is_verification_failure(code, output):
        return "unproved"
    errors = [line for line in output.splitlines() if line.startswith("error")
              and not line.startswith("error: aborting")]
    if code != 0 and errors and any(unsupported in line for line in errors) \
            and all(" is not supported" in line for line in errors):
        return "unsupported"
    raise ValueError(output)


def main():
    version = subprocess.run(["verus", "--version"], text=True,
                             capture_output=True, check=True, timeout=30)
    report = {"verus": version.stdout.strip(), "probes": {}}
    with tempfile.TemporaryDirectory(prefix="xlfn-sync-contracts-") as directory:
        for name, (source, unsupported) in PROBES.items():
            path = Path(directory) / (name + ".rs")
            path.write_text("use vstd::prelude::*;\nverus! {\n" + source + "\n}\n")
            result = subprocess.run(["verus", "--crate-type=lib", str(path)],
                                    text=True, capture_output=True, timeout=120)
            try:
                report["probes"][name] = classify(
                    result.returncode, result.stdout + result.stderr, unsupported)
            except ValueError as error:
                raise SystemExit("Unexpected sync probe failure: " + name + "\n" + str(error))
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
