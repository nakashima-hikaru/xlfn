"""Exercise the installer without downloading a release or changing rustup state."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile


INSTALLER = Path(__file__).with_name("install_verus.sh")
VERSION = "0.2026.09.13.671956e"
TOOLCHAIN = "1.98.1-x86_64-unknown-linux-gnu"

MOCK_COMMAND = """
import json
import os
from pathlib import Path
import shutil
import sys

name = Path(sys.argv[0]).name
args = sys.argv[1:]
if name == "uname":
    print("Linux" if args == ["-s"] else "x86_64")
    raise SystemExit(0)
with Path(os.environ["INSTALLER_LOG"]).open("a") as log:
    log.write(json.dumps({"command": name, "args": args}) + "\\n")
if name == "curl":
    if os.environ.get("CURL_EXIT"):
        raise SystemExit(int(os.environ["CURL_EXIT"]))
    shutil.copyfile(os.environ["INSTALLER_ARCHIVE"], args[args.index("-o") + 1])
elif name == "rustup":
    if os.environ.get("RUSTUP_EXIT"):
        raise SystemExit(int(os.environ["RUSTUP_EXIT"]))
    installed = Path(os.environ["INSTALLED_TOOLCHAINS"])
    toolchains = json.loads(installed.read_text())
    toolchains.append(args[2])
    installed.write_text(json.dumps(toolchains))
elif name == "verus":
    installed = json.loads(Path(os.environ["INSTALLED_TOOLCHAINS"]).read_text())
    if os.environ["REQUIRED_TOOLCHAIN"] not in installed:
        print("verus: required rust toolchain not found", file=sys.stderr)
        raise SystemExit(1)
    if os.environ.get("VERUS_EXIT"):
        raise SystemExit(int(os.environ["VERUS_EXIT"]))
    print("Verus " + os.environ["RELEASE_VERSION"])
else:
    raise SystemExit("unexpected command: " + name)
"""


@unittest.skipUnless(shutil.which("bash") and shutil.which("unzip"),
                     "installer requires bash and unzip")
class InstallVerusTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="xlfn install verus ")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.bin = self.root / "mock commands"
        self.bin.mkdir()
        self.script = "#!" + sys.executable + "\n" + MOCK_COMMAND
        for name in ("uname", "curl", "rustup"):
            path = self.bin / name
            path.write_text(self.script)
            path.chmod(0o755)
        self.log = self.root / "commands.jsonl"
        self.toolchains = self.root / "toolchains.json"
        self.toolchains.write_text(json.dumps(["1.99.0-x86_64-unknown-linux-gnu"]))
        self.archive = self.root / "release.zip"
        self.destination = self.root / "installed verus"
        self.workspace = self.root / "workspace"
        self.workspace.mkdir()
        (self.workspace / "rust-toolchain.toml").write_text(
            '[toolchain]\nchannel = "1.99.0"\n')
        self.env = os.environ.copy()
        for name in ("VERUS_VERSION", "CURL_EXIT", "RUSTUP_EXIT", "VERUS_EXIT"):
            self.env.pop(name, None)
        self.env.update({
            "PATH": str(self.bin) + os.pathsep + self.env["PATH"],
            "INSTALLER_LOG": str(self.log),
            "INSTALLER_ARCHIVE": str(self.archive),
            "INSTALLED_TOOLCHAINS": str(self.toolchains),
            "REQUIRED_TOOLCHAIN": TOOLCHAIN,
            "RELEASE_VERSION": VERSION,
            "RUSTUP_TOOLCHAIN": "1.99.0",
        })
        self.write_archive()

    def write_archive(self, version=VERSION, toolchain=TOOLCHAIN,
                      *, include_metadata=True):
        with zipfile.ZipFile(self.archive, "w") as archive:
            executable = zipfile.ZipInfo("verus-x86-linux/verus")
            executable.create_system = 3
            executable.external_attr = 0o100755 << 16
            archive.writestr(executable, self.script)
            if include_metadata:
                archive.writestr("verus-x86-linux/version.json", json.dumps({
                    "verus": {"version": version, "toolchain": toolchain},
                }))

    def run_installer(self):
        return subprocess.run(
            ["bash", str(INSTALLER), str(self.destination)],
            cwd=self.workspace, env=self.env, text=True, capture_output=True,
            timeout=30,
        )

    def commands(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def assert_no_runtime_install(self, result):
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("installed successfully", result.stdout)
        self.assertEqual([event["command"] for event in self.commands()], ["curl"])
        self.assertFalse(self.destination.exists())

    def test_installs_release_compiler_with_newer_workspace_toolchain(self):
        self.write_archive(toolchain=TOOLCHAIN +
                           " (overridden by environment variable RUSTUP_TOOLCHAIN)")
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        commands = self.commands()
        self.assertEqual([event["command"] for event in commands],
                         ["curl", "rustup", "verus"])
        self.assertEqual(commands[1]["args"],
                         ["toolchain", "install", TOOLCHAIN, "--profile", "minimal"])
        self.assertEqual(commands[2]["args"], ["--version"])
        self.assertIn("installed successfully", result.stdout)

    def test_release_override_uses_its_own_compiler(self):
        version = "0.2026.10.01.abcdef1"
        toolchain = "1.99.0-x86_64-unknown-linux-gnu"
        self.write_archive(version=version, toolchain=toolchain + " (default)")
        self.env.update({"VERUS_VERSION": version, "RELEASE_VERSION": version,
                         "REQUIRED_TOOLCHAIN": toolchain})
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        commands = self.commands()
        self.assertIn("https://github.com/verus-lang/verus/releases/download/"
                      f"release/{version}/verus-{version}-x86-linux.zip",
                      commands[0]["args"])
        self.assertEqual(commands[1]["args"],
                         ["toolchain", "install", toolchain, "--profile", "minimal"])

    def test_compiler_install_failure_stops_before_copying_binaries(self):
        self.env["RUSTUP_EXIT"] = "43"
        result = self.run_installer()
        self.assertEqual(result.returncode, 43)
        self.assertNotIn("installed successfully", result.stdout)
        self.assertEqual([event["command"] for event in self.commands()],
                         ["curl", "rustup"])
        self.assertFalse(self.destination.exists())

    def test_verus_version_failure_is_not_reported_as_success(self):
        self.env["VERUS_EXIT"] = "31"
        result = self.run_installer()
        self.assertEqual(result.returncode, 31)
        self.assertNotIn("installed successfully", result.stdout)
        self.assertEqual(self.commands()[-1],
                         {"command": "verus", "args": ["--version"]})

    def test_missing_release_metadata_fails_before_installation(self):
        self.write_archive(include_metadata=False)
        result = self.run_installer()
        self.assert_no_runtime_install(result)
        self.assertIn("Invalid Verus release metadata", result.stderr)

    def test_invalid_toolchain_metadata_fails_before_installation(self):
        for toolchain in ("", "invalid/toolchain", None):
            with self.subTest(toolchain=toolchain):
                self.write_archive(toolchain=toolchain)
                self.log.unlink(missing_ok=True)
                result = self.run_installer()
                self.assert_no_runtime_install(result)
                self.assertIn("Invalid Verus release metadata", result.stderr)

    def test_mismatched_release_version_fails_before_installation(self):
        self.write_archive(version="unexpected-version")
        result = self.run_installer()
        self.assert_no_runtime_install(result)
        self.assertIn("release version does not match", result.stderr)

    def test_download_failure_is_not_reported_as_success(self):
        self.env["CURL_EXIT"] = "22"
        result = self.run_installer()
        self.assertEqual(result.returncode, 22)
        self.assert_no_runtime_install(result)
        self.assertIn("-fsSL", self.commands()[0]["args"])


if __name__ == "__main__":
    unittest.main()
