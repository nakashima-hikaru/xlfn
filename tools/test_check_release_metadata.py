from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from check_release_metadata import LICENSES, check


class ReleaseMetadataTests(unittest.TestCase):
    def setUp(self):
        self.temporary = TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.write("Cargo.toml", '''
[workspace]
members = ["crates/api", "crates/kernel", "tools/generator"]
[workspace.package]
version = "0.2.0"
publish = true
[workspace.dependencies]
kernel = { version = "=0.1.0", path = "crates/kernel" }
''')
        self.write("crates/api/Cargo.toml", '''
[package]
name = "api"
version.workspace = true
publish.workspace = true
[dependencies]
kernel.workspace = true
''')
        self.write("crates/kernel/Cargo.toml", '''
[package]
name = "kernel"
version = "0.1.0"
''')
        self.write("tools/generator/Cargo.toml", '''
[package]
name = "generator"
version.workspace = true
publish = false
''')
        for name in LICENSES:
            self.write(name, f"Canonical {name}\n")
            for member in ("api", "kernel"):
                self.write(f"crates/{member}/{name}", f"Canonical {name}\n")
        for member in ("api", "kernel"):
            self.write(f"crates/{member}/README.md", f"# {member}\n")

    def write(self, relative: str, content: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def append(self, relative: str, content: str) -> None:
        path = self.root / relative
        self.write(relative, path.read_text(encoding="utf-8") + content)

    def test_valid_mixed_versions_and_unpublished_tool(self):
        self.assertEqual(check(self.root), [])

    def test_missing_readme_and_each_license_are_reported(self):
        for name in ("README.md", *LICENSES):
            path = self.root / "crates/api" / name
            original = path.read_bytes()
            with self.subTest(name=name):
                path.unlink()
                errors = check(self.root)
                self.assertEqual(len(errors), 1)
                self.assertIn(f"crates/api/{name}: missing", errors[0])
                path.write_bytes(original)

    def test_license_drift_is_rejected(self):
        self.write("crates/kernel/LICENSE-MIT", "Different license\n")
        self.assertEqual(check(self.root), [
            "crates/kernel/LICENSE-MIT: differs from repository LICENSE-MIT"
        ])

    def test_workspace_dependency_must_use_actual_member_version(self):
        path = self.root / "Cargo.toml"
        self.write("Cargo.toml", path.read_text().replace('version = "=0.1.0"', 'version = "=0.2.0"'))
        errors = check(self.root)
        self.assertEqual(len(errors), 1)
        self.assertIn("workspace.dependencies.kernel", errors[0])
        self.assertIn("'=0.1.0'", errors[0])

    def test_target_and_build_dependencies_resolve_relative_paths(self):
        self.append("crates/kernel/Cargo.toml", '''
[build-dependencies]
api = { path = "../api", version = "=0.2.0" }
[target.'cfg(windows)'.dev-dependencies]
renamed = { package = "api", path = "../api", version = "=0.1.0" }
''')
        errors = check(self.root)
        self.assertEqual(len(errors), 1)
        self.assertIn("target.cfg(windows).dev-dependencies.renamed", errors[0])
        self.assertIn("'=0.2.0'", errors[0])

    def test_missing_version_and_nonexact_requirement_are_rejected(self):
        self.append("crates/api/Cargo.toml", '''
[dev-dependencies]
missing = { package = "kernel", path = "../kernel" }
[build-dependencies]
ranged = { package = "kernel", path = "../kernel", version = "0.1.0" }
''')
        errors = check(self.root)
        self.assertEqual(len(errors), 2)
        self.assertTrue(any("dependencies.missing" in error for error in errors))
        self.assertTrue(any("dependencies.ranged" in error for error in errors))


if __name__ == "__main__":
    unittest.main()
