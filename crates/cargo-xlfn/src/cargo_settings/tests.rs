use super::*;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[test]
fn nearest_config_and_legacy_name_win_without_changing_path_origins() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    write(
        &root,
        "cargo-home/config.toml",
        "[build]\nrustc-wrapper = 'global'\n[resolver]\nlockfile-path = 'global/Cargo.lock'\n",
    );
    write(
        &root,
        ".cargo/config.toml",
        "[build]\nrustc-wrapper = './parent-wrapper'\n",
    );
    write(
        &root,
        "member/.cargo/config.toml",
        "[build]\nrustc-wrapper = 'ignored-modern'\n[resolver]\nlockfile-path = 'ignored/Cargo.lock'\n",
    );
    write(
        &root,
        "member/.cargo/config",
        "[build]\nrustc-wrapper = './tools/project-wrapper'\n",
    );
    let settings =
        CargoSettings::load_with(&root.join("member"), Some(&root.join("cargo-home")), |_| {
            None
        })
        .unwrap();
    assert_eq!(
        settings.rustc_wrapper,
        Some(root.join("member/./tools/project-wrapper"))
    );
    // The inherited lockfile path retains the global config's own path root.
    assert_eq!(
        settings.lockfile_path(&root),
        root.join("global/Cargo.lock")
    );
}

#[test]
fn includes_merge_in_order_with_the_including_file_last() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    write(
        &root,
        ".cargo/config.toml",
        "include = ['first.toml', 'nested/second.toml', { path = 'missing.toml', optional = true }]\n[build]\nrustc-wrapper = 'outer-wrapper'\n",
    );
    write(
        &root,
        ".cargo/first.toml",
        "[build]\nrustc-wrapper = 'first-wrapper'\n[resolver]\nlockfile-path = 'first/Cargo.lock'\n",
    );
    write(
        &root,
        ".cargo/nested/second.toml",
        "include = ['third.toml']\n[resolver]\nlockfile-path = 'last/Cargo.lock'\n",
    );
    write(
        &root,
        ".cargo/nested/third.toml",
        "[build]\nrustc-wrapper = './third-wrapper'\n",
    );
    let settings = CargoSettings::load_with(&root, None, |_| None).unwrap();
    assert_eq!(settings.rustc_wrapper, Some(PathBuf::from("outer-wrapper")));
    assert_eq!(
        settings.lockfile_path(&root),
        root.join(".cargo/last/Cargo.lock")
    );
}

#[test]
fn include_cycles_and_missing_required_files_fail() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    write(&root, ".cargo/config.toml", "include = ['other.toml']\n");
    assert!(CargoSettings::load_with(&root, None, |_| None).is_err());
    write(&root, ".cargo/other.toml", "include = ['config.toml']\n");
    assert!(CargoSettings::load_with(&root, None, |_| None).is_err());
    write(
        &root,
        ".cargo/other.toml",
        "[build]\nrustc-wrapper = 'wrapper'\n",
    );
    write(
        &root,
        ".cargo/config.toml",
        "include = ['other.toml', './other.toml']\n",
    );
    assert!(CargoSettings::load_with(&root, None, |_| None).is_err());
}

#[test]
fn environment_paths_and_empty_wrapper_follow_cargo_precedence() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    write(
        &root,
        ".cargo/config.toml",
        "[build]\nrustc-wrapper = './config-wrapper'\n[resolver]\nlockfile-path = 'config/Cargo.lock'\n",
    );
    for (environment, expected) in [
        (
            vec![("CARGO_BUILD_RUSTC_WRAPPER", "./env-wrapper")],
            root.join("./env-wrapper"),
        ),
        (
            vec![
                ("CARGO_BUILD_RUSTC_WRAPPER", "ignored"),
                ("RUSTC_WRAPPER", "priority"),
            ],
            PathBuf::from("priority"),
        ),
        (
            vec![
                ("CARGO_BUILD_RUSTC_WRAPPER", "ignored"),
                ("RUSTC_WRAPPER", ""),
            ],
            PathBuf::new(),
        ),
    ] {
        let settings = CargoSettings::load_with(&root, None, |key| {
            if key == "CARGO_RESOLVER_LOCKFILE_PATH" {
                Some("env/Cargo.lock".into())
            } else {
                environment
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| (*value).into())
            }
        })
        .unwrap();
        assert_eq!(settings.rustc_wrapper, Some(expected));
        assert_eq!(settings.lockfile_path(&root), root.join("env/Cargo.lock"));
    }
}

#[test]
fn cargo_home_is_not_reapplied_above_its_ancestor_position() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    write(
        &root,
        ".cargo/config.toml",
        "[build]\nrustc-wrapper = 'home-wrapper'\n",
    );
    write(
        &root,
        "member/.cargo/config.toml",
        "[build]\nrustc-wrapper = 'member-wrapper'\n",
    );
    let settings =
        CargoSettings::load_with(&root.join("member"), Some(&root.join(".cargo")), |_| None)
            .unwrap();
    assert_eq!(
        settings.rustc_wrapper,
        Some(PathBuf::from("member-wrapper"))
    );
}

#[test]
fn lockfile_location_matches_cargo_and_completed_build_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    write(
        &root,
        "Cargo.toml",
        "[package]\nname = 'configured-lockfile'\nversion = '0.0.0'\nedition = '2024'\n[lib]\ncrate-type = ['cdylib']\n",
    );
    write(&root, "src/lib.rs", "pub fn value() -> u32 { 1 }\n");
    write(&root, ".cargo/config.toml", "include = ['locks.toml']\n");
    write(
        &root,
        ".cargo/locks.toml",
        "[resolver]\nlockfile-path = 'config-lock/Cargo.lock'\n",
    );
    // The default path must not accidentally supply the recorded digest.
    write(&root, "Cargo.lock", "unrelated stale default lockfile\n");
    for environment_path in [None, Some("environment-lock/Cargo.lock")] {
        let settings = CargoSettings::load_with(&root, None, |key| {
            (key == "CARGO_RESOLVER_LOCKFILE_PATH")
                .then_some(environment_path)
                .flatten()
                .map(Into::into)
        })
        .unwrap();
        let mut metadata_command = MetadataCommand::new();
        metadata_command
            .current_dir(&root)
            .other_options(vec!["--offline".into()]);
        if let Some(path) = environment_path {
            metadata_command.env("CARGO_RESOLVER_LOCKFILE_PATH", path);
        }
        let graph = metadata_command.exec().unwrap();
        let package = graph.root_package().unwrap();
        let lockfile = settings.lockfile_path(graph.workspace_root.as_std_path());
        assert!(lockfile.is_file());
        let project = ProjectMetadata {
            package_id: package.id.clone(),
            package_name: package.name.to_string(),
            package_version: package.version.to_string(),
            lib_name: package.targets[0].name.clone(),
            artifact_name: package.name.to_string(),
            manifest_path: package.manifest_path.as_std_path().to_path_buf(),
            manifest_directory: root.clone(),
            target_directory: root.join("target"),
            crt: ResolvedCrtPolicy::resolve(Some(CrtPolicy::Inherit), None),
            lockfile_path: lockfile.clone(),
            rustc_wrapper: None,
            bundle: None,
        };
        let mut command = cargo_command();
        command.current_dir(&root).args(["build", "--offline"]);
        if let Some(path) = environment_path {
            command.env("CARGO_RESOLVER_LOCKFILE_PATH", path);
        }
        let built =
            run_cargo_build(&mut command, &project, std::env::consts::DLL_EXTENSION).unwrap();
        let digest = xlfn_package::sha256(&lockfile).unwrap();
        assert_eq!(built.lockfile_sha256, Some(digest));
        assert_ne!(
            built.lockfile_sha256,
            Some(xlfn_package::sha256(&root.join("Cargo.lock")).unwrap())
        );
    }
}
