//! Exercise the real CLI and internal compiler wrapper. A build script stops
//! after checking the wrapper chain, before any Windows linker is needed.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn wrapper(root: &Path, name: &str, cfg: &str) {
    write(root, name, &format!("#!/bin/sh\nexec \"$@\" --cfg {cfg}\n"));
    fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn cli_preserves_configured_and_environment_wrappers_for_every_crt_policy() {
    for policy in ["inherit", "static", "dynamic"] {
        for (source, selected_cfg) in [
            ("config", "project_wrapper"),
            ("build-env", "build_env_wrapper"),
            ("rustc-env", "rustc_env_wrapper"),
            ("disabled", ""),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            write(
                root,
                "Cargo.toml",
                "[package]\nname = 'wrapper-fixture'\nversion = '0.0.0'\nedition = '2024'\n[lib]\ncrate-type = ['cdylib']\n",
            );
            write(root, "src/lib.rs", "pub fn value() -> u32 { 1 }\n");
            write(root, ".cargo/config.toml", "include = ['compiler.toml']\n");
            write(
                root,
                ".cargo/compiler.toml",
                "[build]\nrustc-wrapper = './project wrapper.sh'\nrustc-workspace-wrapper = './workspace.sh'\n",
            );
            wrapper(root, "project wrapper.sh", "project_wrapper");
            wrapper(root, "build-env.sh", "build_env_wrapper");
            wrapper(root, "rustc-env.sh", "rustc_env_wrapper");
            wrapper(root, "workspace.sh", "workspace_wrapper");
            let expected = if selected_cfg.is_empty() {
                "all(workspace_wrapper, not(project_wrapper), not(build_env_wrapper), not(rustc_env_wrapper))".to_owned()
            } else {
                format!("all(workspace_wrapper, {selected_cfg})")
            };
            write(
                root,
                "build.rs",
                &format!(
                    "#[cfg(not({expected}))]\ncompile_error!(\"EXPECTED_WRAPPER_WAS_SKIPPED\");\nfn main() {{ panic!(\"WRAPPER_CHAIN_CONFIRMED\"); }}\n"
                ),
            );
            let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-xlfn"));
            command
                .current_dir(root)
                .args([
                    "check",
                    "--offline",
                    "--crt",
                    policy,
                    "--target",
                    "x86_64-pc-windows-msvc",
                ])
                .env_remove("RUSTC_WRAPPER")
                .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
                .env_remove("RUSTC_WORKSPACE_WRAPPER")
                .env_remove("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER")
                .env_remove("CARGO_RESOLVER_LOCKFILE_PATH");
            if source != "config" {
                command.env("CARGO_BUILD_RUSTC_WRAPPER", "./build-env.sh");
            }
            if source == "rustc-env" {
                command.env("RUSTC_WRAPPER", "./rustc-env.sh");
            } else if source == "disabled" {
                command.env("RUSTC_WRAPPER", "");
            }
            let output = command.output().unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success());
            assert!(
                stderr.contains("WRAPPER_CHAIN_CONFIRMED"),
                "{policy}/{source}: {stderr}"
            );
            assert!(
                !stderr.contains("EXPECTED_WRAPPER_WAS_SKIPPED"),
                "{policy}/{source}: {stderr}"
            );
        }
    }
}
