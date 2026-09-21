#!/usr/bin/env python3
"""Report native Box/Drop support separately from permission-backed ownership.

This diagnostic does not require unsupported APIs to stay unsupported. It never
counts compiler rejection as a proof or as a successful correctness mutation.
Unexpected diagnostics are errors, not evidence of a known support boundary.
"""
from pathlib import Path
import json
import subprocess
import tempfile

from check_drain_gate_refinement import is_verification_failure

PROBES = {
    "box_new_value": ("""
fn probe() {
    let value = Box::new(7u8);
    assert(*value == 7);
}
""", ()),
    "box_into_raw": ("""
fn probe() -> *mut u8 { Box::into_raw(Box::new(7u8)) }
""", ("::into_raw` is not supported",)),
    "box_leak_to_pointer": ("""
fn probe() -> *mut u8 { Box::leak(Box::new(7u8)) as *mut u8 }
""", ("does not yet support the following Rust feature: dereferencing a pointer",)),
    "box_raw_round_trip": ("""
fn probe() {
    let pointer = Box::into_raw(Box::new(7u8));
    let value = unsafe { Box::from_raw(pointer) };
    assert(*value == 7);
}
""", ("::into_raw` is not supported", "::from_raw` is not supported")),
    "permission_borrow_value": ("""
fn probe(pointer: *mut u8, Tracked(memory): Tracked<&vstd::raw_ptr::PointsTo<u8>>)
    -> (value: u8)
    requires memory.ptr() == pointer, memory.is_init(),
    ensures value == memory.value(),
{
    *vstd::raw_ptr::ptr_ref(pointer, Tracked(memory))
}
""", ()),
}


def classify(code, output, unsupported):
    if code == 0 and "verification results:: 1 verified, 0 errors" in output:
        return "proved"
    if is_verification_failure(code, output):
        return "unproved"
    errors = [line for line in output.splitlines() if line.startswith("error")
              and not line.startswith("error: aborting")]
    if code != 0 and unsupported and errors and all(
        any(marker in line for marker in unsupported) for line in errors
    ) and all(any(marker in line for line in errors) for marker in unsupported):
        return "unsupported"
    raise ValueError(output)


def main():
    version = subprocess.run(["verus", "--version"], text=True, capture_output=True,
                             check=True, timeout=30)
    report = {"verus": version.stdout.strip(), "probes": {}}
    with tempfile.TemporaryDirectory(prefix="xlfn-heap-contracts-") as directory:
        for name, (source, unsupported) in PROBES.items():
            path = Path(directory) / (name + ".rs")
            path.write_text("use vstd::prelude::*;\nverus! {\n" + source + "\n}\n")
            result = subprocess.run(["verus", "--crate-type=lib", str(path)],
                                    text=True, capture_output=True, timeout=120)
            try:
                report["probes"][name] = classify(
                    result.returncode, result.stdout + result.stderr, unsupported)
            except ValueError as error:
                raise SystemExit("Unexpected heap probe failure: " + name + "\n" + str(error))
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
