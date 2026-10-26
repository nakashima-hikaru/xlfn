"""Exercise the installer without downloading a release or changing rustup state."""

import hashlib
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
VERSION = "0.2026.10.26.d17fbd0"
# Keep the fixture compiler older than the workspace to exercise independent selection.
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
    print(os.environ.get("MOCK_OS", "Linux") if args == ["-s"] else os.environ.get("MOCK_ARCH", "x86_64"))
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
elif name == "xattr":
    pass
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
        for name in ("uname", "curl", "rustup", "xattr"):
            path = self.bin / name
            path.write_text(self.script)
            path.chmod(0o755)
        self.installer = self.root / "install_verus.sh"
        shutil.copyfile(INSTALLER, self.installer)
        self.digests = self.root / "verus_release_sha256.json"
        self.log = self.root / "commands.jsonl"
        self.toolchains = self.root / "toolchains.json"
        self.toolchains.write_text(json.dumps(["1.99.1-x86_64-unknown-linux-gnu"]))
        self.archive = self.root / "release.zip"
        self.destination = self.root / "installed verus"
        self.workspace = self.root / "workspace"
        self.workspace.mkdir()
        (self.workspace / "rust-toolchain.toml").write_text(
            '[toolchain]\nchannel = "1.99.1"\n')
        self.env = os.environ.copy()
        for name in ("VERUS_VERSION", "CURL_EXIT", "RUSTUP_EXIT", "VERUS_EXIT", "MOCK_OS", "MOCK_ARCH"):
            self.env.pop(name, None)
        self.env.update({
            "PATH": str(self.bin) + os.pathsep + self.env["PATH"],
            "INSTALLER_LOG": str(self.log),
            "INSTALLER_ARCHIVE": str(self.archive),
            "INSTALLED_TOOLCHAINS": str(self.toolchains),
            "REQUIRED_TOOLCHAIN": TOOLCHAIN,
            "RELEASE_VERSION": VERSION,
            "RUSTUP_TOOLCHAIN": "1.99.1",
        })
        self.write_archive()

    def write_archive(self, version=VERSION, toolchain=TOOLCHAIN,
                      *, include_metadata=True):
        with zipfile.ZipFile(self.archive, "w") as archive:
            platform = "arm64-macos" if self.env.get("MOCK_OS") == "Darwin" else "x86-linux"
            folder = "verus-" + platform
            executable = zipfile.ZipInfo(folder + "/verus")
            executable.create_system = 3
            executable.external_attr = 0o100755 << 16
            archive.writestr(executable, self.script)
            if include_metadata:
                archive.writestr(folder + "/version.json", json.dumps({
                    "verus": {"version": version, "toolchain": toolchain},
                }))

            extension = "dylib" if platform.endswith("macos") else "so"
            for name in ("rust_verify", "z3", "libverus_builtin.rlib",
                         "libverus_builtin_macros." + extension,
                         "libverus_state_machines_macros." + extension,
                         "libvstd.rlib", "vstd.vir", "unrelated-binary"):
                archive.writestr(folder + "/" + name, "fixture")
        self.pin_archive(version=version, platform=platform)

    def pin_archive(self, *, version=VERSION, platform="x86-linux"):
        self.digests.write_text(json.dumps({version: {
            f"verus-{version}-{platform}.zip": hashlib.sha256(self.archive.read_bytes()).hexdigest(),
        }}))

    def run_installer(self):
        return subprocess.run(
            ["bash", str(self.installer), str(self.destination)],
            cwd=self.workspace, env=self.env, text=True, capture_output=True,
            timeout=30,
        )

    def commands(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

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
        self.pin_archive()
        result = self.run_installer()
        self.assert_no_runtime_install(result)
        self.assertIn("release version does not match", result.stderr)

    def test_download_failure_is_not_reported_as_success(self):
        self.env["CURL_EXIT"] = "22"
        result = self.run_installer()
        self.assertEqual(result.returncode, 22)
        self.assert_no_runtime_install(result)
        self.assertIn("-fsSL", self.commands()[0]["args"])

    def test_unknown_version_requires_reviewed_digest_before_download(self):
        self.env["VERUS_VERSION"] = "unknown"
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("No trusted Verus SHA-256", result.stderr)
        self.assertEqual(self.commands(), [])
        self.assertFalse(self.destination.exists())

    def test_checksum_mismatch_stops_before_extraction_or_execution(self):
        self.archive.write_bytes(b"not a zip archive")
        result = self.run_installer()
        self.assert_no_runtime_install(result)
        self.assertIn("SHA-256 mismatch", result.stderr)
        self.assertNotIn("Extracting", result.stdout)

    def test_existing_destination_and_unrelated_binaries_are_preserved(self):
        self.destination.mkdir()
        unrelated = self.destination / "cargo"
        unrelated.write_bytes(b"original binary")
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(unrelated.read_bytes(), b"original binary")
        self.assertEqual(self.commands(), [])

    def test_symlink_destination_is_rejected(self):
        other = self.root / "other binaries"
        other.mkdir()
        self.destination.symlink_to(other, target_is_directory=True)
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(list(other.iterdir()), [])
        self.assertEqual(self.commands(), [])

    def test_only_explicit_runtime_files_are_installed(self):
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse((self.destination / "unrelated-binary").exists())
        self.assertEqual(len(list(self.destination.iterdir())), 9)

    def test_macos_quarantine_removal_only_touches_installed_files(self):
        self.env.update({"MOCK_OS": "Darwin", "MOCK_ARCH": "arm64"})
        self.write_archive()
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        events = [event for event in self.commands() if event["command"] == "xattr"]
        self.assertEqual(len(events), 9)
        for event in events:
            self.assertEqual(event["args"][:2], ["-d", "com.apple.quarantine"])
            self.assertEqual(Path(event["args"][2]).parent, self.destination)
            self.assertTrue(Path(event["args"][2]).is_file())

    def test_missing_runtime_component_is_rejected_before_copy(self):
        with zipfile.ZipFile(self.archive) as source:
            contents = {info.filename: (info, source.read(info)) for info in source.infolist()
                        if not info.filename.endswith("/z3")}
        with zipfile.ZipFile(self.archive, "w") as target:
            for info, data in contents.values():
                target.writestr(info, data)
        self.pin_archive()
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Missing or invalid Verus runtime component: z3", result.stderr)
        self.assertFalse(self.destination.exists())


if __name__ == "__main__":
    unittest.main()
