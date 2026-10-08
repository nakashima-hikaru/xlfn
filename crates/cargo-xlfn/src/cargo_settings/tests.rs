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
        "[build]\nrustc-wrapper = 'global'\n",
    );
    write(
        &root,
        ".cargo/config.toml",
        "[build]\nrustc-wrapper = './parent-wrapper'\n",
    );
    write(
        &root,
        "member/.cargo/config.toml",
        "[build]\nrustc-wrapper = 'ignored-modern'\n",
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
        "[build]\nrustc-wrapper = 'first-wrapper'\n",
    );
    write(
        &root,
        ".cargo/nested/second.toml",
        "include = ['third.toml']\n",
    );
    write(
        &root,
        ".cargo/nested/third.toml",
        "[build]\nrustc-wrapper = './third-wrapper'\n",
    );
    let settings = CargoSettings::load_with(&root, None, |_| None).unwrap();
    assert_eq!(settings.rustc_wrapper, Some(PathBuf::from("outer-wrapper")));
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
        "[build]\nrustc-wrapper = './config-wrapper'\n",
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
            environment
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).into())
        })
        .unwrap();
        assert_eq!(settings.rustc_wrapper, Some(expected));
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
