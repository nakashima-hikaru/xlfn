use super::*;

fn parse_cli_args<'a>(args: &'a [&'a str]) -> std::result::Result<Cli, usage::Error<'static, 'a>> {
    let argv: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
    Cli::parse_from(&argv)
}

#[test]
fn cargo_subcommand_name_is_removed_before_cli_parsing() {
    let args = normalize_cargo_subcommand_args(
        [
            "cargo-xlfn",
            "xlfn",
            "check",
            "--target",
            "x86_64-pc-windows-msvc",
        ]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect(),
    );
    let argv: Vec<&std::ffi::OsStr> = args.iter().map(std::ops::Deref::deref).collect();
    let parsed = Cli::parse_from_argv(&argv).unwrap();
    assert!(matches!(parsed.command, Commands::Check(_)));
}

#[test]
fn removed_native_commands_and_unknown_targets_are_rejected() {
    assert!(parse_cli_args(&["native", "inspect"]).is_err());
    assert!(parse_cli_args(&["new", "my-xll"]).is_err());
    assert!(parse_cli_args(&["package", "--target", "x86_64-pc-window-msvc"]).is_err());
}

#[test]
fn transactional_distribution_requires_a_dedicated_output_directory() {
    assert!(validate_transactional_output_root(Path::new(".")).is_err());
    assert!(validate_transactional_output_root(Path::new("..")).is_err());
    assert!(validate_transactional_output_root(Path::new("/")).is_err());
    assert!(validate_transactional_output_root(Path::new("dist")).is_ok());
}

#[test]
fn package_output_and_build_directories_must_not_overlap() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for (output, target) in [
        ("dist", "dist"),
        ("dist", "dist/cache"),
        ("dist/package", "dist"),
        ("dist/./package/..", "dist/cache"),
    ] {
        let error =
            validate_output_target_separation(&root.join(output), &root.join(target)).unwrap_err();
        assert!(error.to_string().contains("must not overlap"));
    }
    validate_output_target_separation(&root.join("dist"), &root.join("target")).unwrap();
    validate_output_target_separation(&root.join("dist"), &root.join("dist-cache")).unwrap();
    assert!(!root.join("dist").exists());
    assert!(!root.join("target").exists());
}

#[test]
fn package_rejects_overlap_before_creating_build_storage_or_replacing_output() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = 'overlap-probe'\nversion = '0.0.0'\nedition = '2024'\n\
         [lib]\ncrate-type = ['cdylib']\npath = 'lib.rs'\n",
    )
    .unwrap();
    fs::write(root.join("lib.rs"), "").unwrap();
    let output = root.join("dist");
    fs::create_dir(&output).unwrap();
    fs::write(output.join("keep.txt"), b"existing distribution").unwrap();
    for all in [false, true] {
        let error = package(&PackageArgs {
            all,
            target: (!all).then_some(WindowsTarget::X64),
            manifest_path: Some(root.join("Cargo.toml")),
            out: output.clone(),
            target_dir: Some(output.join("cache")),
            offline: true,
            ..Default::default()
        })
        .unwrap_err();
        assert!(error.to_string().contains("must not overlap"));
        assert_eq!(
            fs::read(output.join("keep.txt")).unwrap(),
            b"existing distribution"
        );
        assert!(!output.join("cache").exists());
        assert_eq!(fs::read_dir(&output).unwrap().count(), 1);
    }
    if root.join("DIST").exists() {
        // The volume folds case. The two fresh roots do not exist until the
        // build ancestor is created, so path text alone cannot detect this.
        let fresh_output = root.join("fresh-dist");
        let target = root.join("FRESH-DIST/cache");
        let error = package(&PackageArgs {
            all: true,
            manifest_path: Some(root.join("Cargo.toml")),
            out: fresh_output,
            target_dir: Some(target.clone()),
            offline: true,
            ..Default::default()
        })
        .unwrap_err();
        assert!(error.to_string().contains("must not overlap"));
        let target_parent = crt::ResolvedCrtPolicy::resolve(None, None).target_directory(&target);
        assert_eq!(fs::read_dir(target_parent).unwrap().count(), 0);
    }
}

#[cfg(unix)]
#[test]
fn package_overlap_check_resolves_target_symlinks() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::create_dir(root.join("dist")).unwrap();
    std::os::unix::fs::symlink(root.join("dist"), root.join("target-alias")).unwrap();
    let error =
        validate_output_target_separation(&root.join("dist"), &root.join("target-alias/cache"))
            .unwrap_err();
    assert!(error.to_string().contains("must not overlap"));
}

#[cfg(unix)]
#[test]
fn package_overlap_check_needs_only_search_permission_on_ancestors() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let ancestor = root.join("search-only");
    let output = ancestor.join("dist");
    let target = ancestor.join("target");
    fs::create_dir_all(&output).unwrap();
    fs::create_dir_all(&target).unwrap();
    let permissions = fs::metadata(&ancestor).unwrap().permissions();
    fs::set_permissions(&ancestor, std::fs::Permissions::from_mode(0o111)).unwrap();
    let read_denied = fs::read_dir(&ancestor).is_err();
    let children_accessible = output.is_dir() && target.is_dir();
    let result = validate_output_target_separation(&output, &target);
    // Restore access before any assertion, including on hosts where elevated
    // test privileges make permission denial impossible to reproduce.
    fs::set_permissions(&ancestor, permissions).unwrap();
    if read_denied {
        assert!(children_accessible);
        result.unwrap();
    }
}

#[test]
fn generated_distribution_basenames_are_reserved_case_insensitively() {
    assert!(is_reserved_distribution_name("calcaddin.XLL", "CalcAddin"));
    assert!(is_reserved_distribution_name(
        "BUILD-MANIFEST.JSON",
        "CalcAddin"
    ));
    assert!(!is_reserved_distribution_name(
        "CalcEngine.dll",
        "CalcAddin"
    ));
}

#[test]
fn windows_basename_validator_rejects_invalid_names() {
    assert!(validate_windows_basename("foo:bar").is_err());
    assert!(validate_windows_basename("foo*").is_err());
    assert!(validate_windows_basename("foo?").is_err());
    assert!(validate_windows_basename("name.").is_err());
    assert!(validate_windows_basename("name ").is_err());
    assert!(validate_windows_basename("CON").is_err());
    assert!(validate_windows_basename("AUX").is_err());
    assert!(validate_windows_basename("NUL").is_err());
    assert!(validate_windows_basename("COM1").is_err());
    assert!(validate_windows_basename("valid-artifact-name").is_ok());
}

#[test]
fn bundle_metadata_type_error_reports_nested_path() {
    let error = parse_bundle_metadata(serde_json::json!({
        "x86": ["foo.dll", 42]
    }))
    .unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains("[package.metadata.xlfn.bundle]"),
        "{message}"
    );
    assert!(message.contains("x86[1]"), "{message}");
}

#[test]
fn bundle_metadata_scalar_error_reports_field_path() {
    let error = parse_bundle_metadata(serde_json::json!({
        "strict-paths": "yes"
    }))
    .unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains("[package.metadata.xlfn.bundle]"),
        "{message}"
    );
    assert!(message.contains("strict-paths"), "{message}");
}

#[test]
fn bundle_metadata_path_tracking_preserves_valid_deserialization() {
    let metadata = parse_bundle_metadata(serde_json::json!({
        "x86": ["foo.dll"],
        "x64": ["bar.dll"],
        "external-imports": ["engine.dll"],
        "strict-paths": true
    }))
    .unwrap();

    assert_eq!(metadata.x86, vec!["foo.dll"]);
    assert_eq!(metadata.x64, vec!["bar.dll"]);
    assert_eq!(metadata.external_imports, vec!["engine.dll"]);
    assert!(metadata.strict_paths);
}

#[test]
fn check_args_supports_build_selection_flags() {
    let parsed = parse_cli_args(&[
        "check",
        "--profile",
        "release",
        "--features",
        "feat1,feat2",
        "--crt",
        "dynamic",
        "--locked",
    ])
    .unwrap();
    let Commands::Check(args) = parsed.command else {
        panic!("expected Check command");
    };
    let build = args.build();
    assert_eq!(build.profile.as_deref(), Some("release"));
    assert_eq!(build.normalized_features(), vec!["feat1", "feat2"]);
    assert_eq!(build.crt, Some(CrtPolicy::Dynamic));
    assert!(build.locked);
}

#[test]
fn cli_accepts_every_crt_policy() {
    for policy in ["inherit", "static", "dynamic"] {
        assert!(
            parse_cli_args(&["check", "--crt", policy]).is_ok(),
            "policy {policy} should parse"
        );
    }
    assert!(parse_cli_args(&["check", "--crt", "auto"]).is_err());
}

#[test]
fn metadata_uses_the_same_feature_and_resolution_constraints_as_build() {
    let selection = BuildSelectionArgs {
        features: vec!["feat1".to_owned(), "feat2".to_owned()],
        no_default_features: true,
        all_features: false,
        locked: true,
        frozen: false,
        offline: true,
        ..BuildSelectionArgs::default()
    };
    let mut metadata = MetadataCommand::new();
    selection.apply_to_metadata(&mut metadata);
    let command = metadata.cargo_command();
    let arguments = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(
        arguments
            .array_windows::<2>()
            .any(|[flag, value]| flag == "--features" && value == "feat1,feat2")
    );
    assert!(
        arguments
            .iter()
            .any(|argument| argument == "--no-default-features")
    );
    assert!(arguments.iter().any(|argument| argument == "--locked"));
    assert!(arguments.iter().any(|argument| argument == "--offline"));
}

#[test]
fn feature_selection_normalizes_cargo_separators_for_metadata_and_build() {
    let selection = BuildSelectionArgs {
        features: vec!["rtd,async handles".into(), "async\tcache".into()],
        ..BuildSelectionArgs::default()
    };
    assert_eq!(
        selection.normalized_features(),
        ["async", "cache", "handles", "rtd"]
    );
    let mut metadata = MetadataCommand::new();
    selection.apply_to_metadata(&mut metadata);
    let mut build = Command::new("cargo");
    selection.apply_to_command(&mut build, None);
    for command in [metadata.cargo_command(), build] {
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            arguments.array_windows::<2>().any(|[flag, value]| {
                flag == "--features" && value == "async,cache,handles,rtd"
            })
        );
    }
}

#[test]
fn base_cargo_command_does_not_rewrite_rustflags() {
    let command = cargo_command();
    let arguments = command
        .get_args()
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>();
    assert!(arguments.is_empty());
    for (name, value) in command.get_envs() {
        assert!(
            name != "RUSTFLAGS" && name != "CARGO_ENCODED_RUSTFLAGS",
            "base cargo command must inherit {name:?}, not override it with {value:?}"
        );
    }
}

#[test]
fn configure_build_sets_target_dir_and_build_dir() {
    let metadata = ProjectMetadata {
        package_id: cargo_metadata::PackageId {
            repr: "test-pkg".into(),
        },
        package_name: "test-pkg".into(),
        package_version: "0.1.0".into(),
        lib_name: "test_pkg".into(),
        artifact_name: "test_pkg".into(),
        manifest_path: PathBuf::from("Cargo.toml"),
        manifest_directory: PathBuf::from("."),
        target_directory: PathBuf::from("target"),
        crt: crt::ResolvedCrtPolicy::resolve(Some(crt::CrtPolicy::Static), None),
        rustc_wrapper: None,
        bundle: None,
    };
    let mut command = cargo_command();
    let temp_target = Path::new("target/temp-target");
    configure_build(
        &mut command,
        &metadata,
        "x86_64-pc-windows-msvc",
        temp_target,
    )
    .expect("configure_build succeeds");
    let args = command
        .get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(
        args.array_windows::<2>()
            .any(|[flag, val]| flag == "--target-dir" && val == "target/temp-target")
    );
    let env_build_dir = command
        .get_envs()
        .find(|(k, _)| *k == "CARGO_BUILD_BUILD_DIR")
        .and_then(|(_, v)| v.map(|s| s.to_string_lossy().into_owned()));
    assert_eq!(
        env_build_dir,
        Some(
            metadata
                .crt
                .target_directory(&metadata.target_directory)
                .join("build-cache")
                .to_string_lossy()
                .into_owned()
        )
    );
}

#[test]
fn workspace_manifest_path_resolves_without_explicit_package() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/basic-xll/Cargo.toml");
    let args = ProjectArgs {
        package: None,
        manifest_path: Some(manifest),
    };
    let build = BuildSelectionArgs::default();
    let metadata = project_metadata(&args, &build).unwrap();
    assert_eq!(metadata.package_name, "basic-xlfn");
}

#[test]
fn project_metadata_rejects_malformed_xlfn_settings() {
    for (metadata, expected) in [
        (
            "[package.metadata]\nxlfn = 42",
            "[package.metadata.xlfn] must be a table",
        ),
        (
            "[package.metadata]\nxlfn = []",
            "[package.metadata.xlfn] must be a table",
        ),
        (
            "[package.metadata.xlfn]\nartifact-name = 42",
            "[package.metadata.xlfn].artifact-name must be a string",
        ),
        (
            "[package.metadata.xlfn]\nartifact-name = false",
            "[package.metadata.xlfn].artifact-name must be a string",
        ),
    ] {
        let error = project_metadata_fixture(metadata)
            .err()
            .expect("malformed settings must not silently use defaults");
        assert!(error.to_string().contains(expected), "{error:#}");
    }
}

#[test]
fn project_metadata_rejects_unknown_settings_instead_of_using_defaults() {
    for (metadata, expected) in [
        (
            "[package.metadata.xlfn]\nartifact_name = \"DataTools\"",
            "unknown [package.metadata.xlfn] setting \"artifact_name\"",
        ),
        (
            "[package.metadata.xlfn]\nCRT = \"dynamic\"",
            "unknown [package.metadata.xlfn] setting \"CRT\"",
        ),
        (
            "[package.metadata.xlfn.bundel]\nx64 = [\"engine.dll\"]",
            "unknown [package.metadata.xlfn] setting \"bundel\"",
        ),
        (
            "[package.metadata.xlfn.bundle]\nexternal_imports = [\"engine.dll\"]",
            "unknown field `external_imports`",
        ),
    ] {
        let error = project_metadata_fixture(metadata)
            .err()
            .expect("unknown settings must not silently use defaults");
        assert!(error.to_string().contains(expected), "{error:#}");
    }
}

#[test]
fn project_metadata_preserves_explicit_and_default_artifact_names() {
    assert_eq!(
        project_metadata_fixture("").unwrap().artifact_name,
        "metadata-diagnostic-fixture"
    );
    assert_eq!(
        project_metadata_fixture("[package.metadata.xlfn]\nartifact-name = \"DataTools\"")
            .unwrap()
            .artifact_name,
        "DataTools"
    );
}

fn project_metadata_fixture(metadata: &str) -> Result<ProjectMetadata> {
    let directory = tempfile::tempdir()?;
    fs::create_dir(directory.path().join("src"))?;
    fs::write(
        directory.path().join("Cargo.toml"),
        format!(
            r#"[package]
name = "metadata-diagnostic-fixture"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

{metadata}
"#,
        ),
    )?;
    fs::write(directory.path().join("src/lib.rs"), "pub fn fixture() {}\n")?;

    let args = ProjectArgs {
        package: None,
        manifest_path: Some(directory.path().join("Cargo.toml")),
    };
    project_metadata(&args, &BuildSelectionArgs::default())
}

#[test]
fn project_metadata_reports_bundle_metadata_path() {
    let error =
        project_metadata_fixture("[package.metadata.xlfn.bundle]\nx86 = [\"foo.dll\", 123]")
            .err()
            .expect("invalid bundle metadata should fail project metadata resolution");
    let message = error.to_string();

    assert!(
        message.contains("[package.metadata.xlfn.bundle]"),
        "{message}"
    );
    assert!(message.contains("x86[1]"), "{message}");
}

#[test]
fn current_directory_selects_the_containing_workspace_member() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let mut command = MetadataCommand::new();
    command.no_deps();
    command.manifest_path(workspace.join("Cargo.toml"));
    let discovery = command.exec().unwrap();
    let args = ProjectArgs::default();

    let package =
        select_discovery_package(&discovery, &args, &workspace.join("crates/xlfn/src")).unwrap();

    assert_eq!(package.name.as_str(), "xlfn");
}

#[test]
fn virtual_workspace_root_remains_ambiguous() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let mut command = MetadataCommand::new();
    command.no_deps();
    command.manifest_path(workspace.join("Cargo.toml"));
    let discovery = command.exec().unwrap();
    let args = ProjectArgs::default();

    let error = select_discovery_package(&discovery, &args, workspace).unwrap_err();

    assert!(error.to_string().contains("--package or --manifest-path"));
}
