#!/usr/bin/env python3
"""Check the RTD feature boundary through an independently featured consumer."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-dir", type=Path, default=ROOT.parents[1] / "target" / "rtd-api-fixture")
    parser.add_argument("--report-dir", type=Path, default=ROOT.parents[1] / "target" / "rtd-api-fixture" / "reports")
    args = parser.parse_args()
    target = args.target_dir.resolve()
    evidence = args.report_dir.resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, CARGO_INCREMENTAL="0")
    combos = [(), ("objects",), ("streaming",), ("objects", "streaming")]
    cases = [("basic", ROOT / "src/lib.rs"), *[(name, ROOT / f"cases/{name}.rs") for name in ("configured", "rtd_exports", "rtd_access", "erased")]]
    report = {
        "rustc": subprocess.check_output(["rustc", "-Vv"], text=True).strip(),
        "environment": {"CARGO_INCREMENTAL": "0"},
        "source_sha256": {str(p.relative_to(ROOT)): digest(p) for p in [ROOT / "Cargo.toml", ROOT / "Cargo.lock", ROOT / "check.py", *[p for _, p in cases]]},
        "checks": [],
    }
    manifest_source = (ROOT / "Cargo.toml").read_text().replace('path = "../../crates/xlfn"', f'path = "{ROOT.parents[1] / "crates/xlfn"}"')
    with tempfile.TemporaryDirectory(prefix="xlfn-rtd-api-consumer-") as directory:
        project = Path(directory)
        (project / "src").mkdir()
        (project / "Cargo.toml").write_text(manifest_source)
        shutil.copyfile(ROOT / "Cargo.lock", project / "Cargo.lock")
        for name, source in cases:
            shutil.copyfile(source, project / "src/lib.rs")
            for features in combos:
                enabled = "streaming" in features
                expected_success = name == "basic" or enabled
                label = f"{name}-{'-'.join(features) or 'core'}"
                command = ["cargo", "check", "--manifest-path", str(project / "Cargo.toml"), "--lib", "--no-default-features", "--locked", "--offline", "--target-dir", str(target), "--message-format=json"]
                if features:
                    command += ["--features", ",".join(features)]
                run = subprocess.run(command, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                errors = []
                for line in run.stdout.splitlines():
                    try:
                        message = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if message.get("reason") == "compiler-message" and message["message"]["level"] == "error":
                        errors.append(message["message"])
                actual_success = run.returncode == 0
                passed = actual_success == expected_success
                if not expected_success:
                    if name == "configured":
                        passed &= len(errors) == 1 and "RTD configuration requires the xlfn `rtd` feature" in errors[0]["message"]
                    elif name == "rtd_exports":
                        codes = [error.get("code", {}).get("code") for error in errors]
                        expected_codes = ["E0603"] * 4 if "objects" in features else ["E0432", "E0603", "E0603", "E0603"]
                        passed &= sorted(codes) == sorted(expected_codes)
                    elif name == "rtd_access":
                        passed &= len(errors) == 1 and errors[0].get("code", {}).get("code") == "E0599" and "rtd" in errors[0]["message"]
                    elif name == "erased":
                        passed &= any("RTD configuration requires the xlfn `rtd` feature" in error["message"] for error in errors)
                log = evidence / f"{label}.log"
                log.write_text((run.stderr + "\n" + "\n".join(error.get("rendered", error["message"]) for error in errors)).rstrip() + "\n")
                report["checks"].append({"case": name, "consumer_features": list(features), "expected_success": expected_success, "exit_code": run.returncode, "passed": passed, "command": command, "diagnostics": [{"message": error["message"], "code": (error.get("code") or {}).get("code")} for error in errors], "log": log.name, "log_sha256": digest(log)})
                print(f"{'PASS' if passed else 'FAIL'} {label}", flush=True)
                (evidence / "verification.json").write_text(json.dumps(report, indent=2) + "\n")
    report["passed"] = all(check["passed"] for check in report["checks"])
    report["passed_checks"] = sum(check["passed"] for check in report["checks"])
    (evidence / "verification.json").write_text(json.dumps(report, indent=2) + "\n")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
