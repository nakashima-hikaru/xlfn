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
fn shared_include_dag_is_rejected_like_cargo() {
    // Cargo's include-cycle detector retains every lexical path loaded from
    // one top-level file, so shared descendants are rejected as well.
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    write(
        &root,
        ".cargo/config.toml",
        "include = ['a.toml', 'b.toml']\n",
    );
    write(&root, ".cargo/a.toml", "include = ['shared.toml']\n");
    write(&root, ".cargo/b.toml", "include = ['shared.toml']\n");
    write(&root, ".cargo/shared.toml", "[build]\nrustc-wrapper = ''\n");
    let error = CargoSettings::load_with(&root, None, |_| None)
        .err()
        .unwrap();
    assert!(error.to_string().contains("include cycle"));
}

#[test]
fn initial_include_path_is_normalized_before_cycle_detection() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    write(&root, ".cargo/config.toml", "include = ['./config.toml']\n");
    let mut seen = Vec::new();
    let error = CargoSettings::read_file(&root.join(".cargo/./config.toml"), &mut seen)
        .err()
        .unwrap();
    assert!(error.to_string().contains("include cycle"));
    assert_eq!(seen, vec![root.join(".cargo/config.toml")]);
}

#[cfg(unix)]
#[test]
fn symlink_alias_cycle_is_rejected_without_changing_path_origins() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    write(
        &root,
        ".cargo/config.toml",
        "include = ['alias/config.toml']\n",
    );
    symlink(root.join(".cargo"), root.join(".cargo/alias")).unwrap();
    let error = CargoSettings::load_with(&root, None, |_| None)
        .err()
        .unwrap();
    assert!(error.to_string().contains("filesystem alias"), "{error}");

    write(
        &root,
        ".cargo/config.toml",
        "include = ['alias/value.toml']\n",
    );
    write(
        &root,
        ".cargo/value.toml",
        "[build]\nrustc-wrapper = './wrapper'\n",
    );
    let settings = CargoSettings::load_with(&root, None, |_| None).unwrap();
    assert_eq!(settings.rustc_wrapper, Some(root.join(".cargo/./wrapper")));
}

#[test]
fn include_depth_is_bounded_and_boundary_is_accepted() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    write(&root, ".cargo/config.toml", "include = ['1.toml']\n");
    for index in 1..MAX_CONFIG_INCLUDE_DEPTH {
        let contents = if index + 1 == MAX_CONFIG_INCLUDE_DEPTH {
            "[build]\nrustc-wrapper = 'deep-wrapper'\n".to_owned()
        } else {
            format!("include = ['{}.toml']\n", index + 1)
        };
        write(&root, &format!(".cargo/{index}.toml"), &contents);
    }
    assert!(CargoSettings::load_with(&root, None, |_| None).is_ok());
    write(
        &root,
        &format!(".cargo/{}.toml", MAX_CONFIG_INCLUDE_DEPTH - 1),
        &format!("include = ['{MAX_CONFIG_INCLUDE_DEPTH}.toml']\n"),
    );
    write(
        &root,
        &format!(".cargo/{MAX_CONFIG_INCLUDE_DEPTH}.toml"),
        "",
    );
    let error = CargoSettings::load_with(&root, None, |_| None)
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("include depth exceeds"),
        "{error}"
    );
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
