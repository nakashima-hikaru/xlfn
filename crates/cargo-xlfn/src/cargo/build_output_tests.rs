use super::*;
use serde_json::{Value, json};

fn metadata() -> ProjectMetadata {
    ProjectMetadata {
        package_id: cargo_metadata::PackageId {
            repr: "selected".into(),
        },
        package_name: "selected".into(),
        package_version: "0.0.0".into(),
        lib_name: "selected".into(),
        artifact_name: "selected".into(),
        manifest_path: PathBuf::from("Cargo.toml"),
        manifest_directory: PathBuf::from("."),
        target_directory: PathBuf::from("target"),
        crt: ResolvedCrtPolicy::resolve(None, None),
        lockfile_path: PathBuf::from("Cargo.lock"),
        bundle: None,
    }
}

fn artifact(package_id: &str, kind: &str, filenames: &[&str]) -> Value {
    json!({
        "reason": "compiler-artifact",
        "package_id": package_id,
        "manifest_path": "/project/Cargo.toml",
        "target": {
            "kind": [kind], "crate_types": [kind], "name": "selected",
            "src_path": "/project/src/lib.rs", "edition": "2024",
            "doc": true, "doctest": false, "test": false
        },
        "profile": {
            "opt_level": "0", "debuginfo": 2, "debug_assertions": true,
            "overflow_checks": true, "test": false
        },
        "features": ["actual", "actual"], "filenames": filenames,
        "executable": null, "fresh": true
    })
}

fn read(messages: &[Value]) -> Result<BuiltLibrary> {
    let stream = messages
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    read_build_messages(stream.as_bytes(), &metadata(), "dll")
}

#[test]
fn selects_the_actual_cdylib_path_and_features_including_fresh_builds() {
    let built = read(&[
        artifact("dependency", "cdylib", &["/dependency.dll"]),
        artifact("selected", "bin", &["/program.dll"]),
        artifact(
            "selected",
            "cdylib",
            &["/custom/place.dll", "/custom/place.dll.lib"],
        ),
        json!({"reason": "build-finished", "success": true}),
    ])
    .unwrap();
    assert_eq!(built.path, Path::new("/custom/place.dll"));
    assert_eq!(built.features, ["actual"]);
}

#[test]
fn incomplete_failed_missing_and_ambiguous_artifacts_are_rejected() {
    let selected = artifact("selected", "cdylib", &["/selected.dll"]);
    for messages in [
        vec![selected.clone()],
        vec![
            selected.clone(),
            json!({"reason": "build-finished", "success": false}),
        ],
        vec![
            artifact("dependency", "cdylib", &["/other.dll"]),
            json!({"reason": "build-finished", "success": true}),
        ],
        vec![
            selected.clone(),
            selected,
            json!({"reason": "build-finished", "success": true}),
        ],
    ] {
        assert!(read(&messages).is_err());
    }
}

#[test]
fn selected_build_features_and_lockfile_come_from_the_completed_build() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for (relative, contents) in [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"addin\", \"other\"]\nresolver = \"3\"\n[profile.artifact-location]\ninherits = \"dev\"\n",
        ),
        (
            "addin/Cargo.toml",
            "[package]\nname = \"provenance-addin\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[lib]\ncrate-type = [\"cdylib\", \"rlib\"]\n[features]\nunrequested = []\nselected = []\n",
        ),
        ("addin/src/lib.rs", "pub fn value() -> u32 { 1 }\n"),
        (
            "other/Cargo.toml",
            "[package]\nname = \"provenance-other\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\nprovenance-addin = { path = \"../addin\", features = [\"unrequested\"] }\n",
        ),
        ("other/src/main.rs", "fn main() {}\n"),
    ] {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    let project = project_metadata(
        &ProjectArgs {
            package: None,
            manifest_path: Some(root.join("addin/Cargo.toml")),
        },
        &BuildSelectionArgs::default(),
    )
    .unwrap();

    // Cargo metadata resolves the entire workspace and includes a feature
    // requested only by an unbuilt member. It is not this cdylib's feature set.
    let graph = MetadataCommand::new()
        .manifest_path(&project.manifest_path)
        .other_options(vec!["--offline".into()])
        .exec()
        .unwrap();
    let resolved = graph
        .resolve
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| node.id == project.package_id)
        .unwrap();
    assert_eq!(resolved.features, ["unrequested"]);
    let old_digest = xlfn_package::sha256(&project.lockfile_path).unwrap();

    // Make Cargo legitimately update its lockfile after metadata discovery.
    fs::create_dir_all(root.join("support/src")).unwrap();
    fs::write(
        root.join("support/Cargo.toml"),
        "[package]\nname = \"provenance-support\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(root.join("support/src/lib.rs"), "pub fn support() {}\n").unwrap();
    let manifest = fs::read_to_string(&project.manifest_path).unwrap();
    fs::write(
        &project.manifest_path,
        format!("{manifest}\n[dependencies]\nprovenance-support = {{ path = \"../support\" }}\n"),
    )
    .unwrap();

    for selected in [false, true, true] {
        let mut command = cargo_command();
        command
            .args(["build", "--offline", "--manifest-path"])
            .arg(&project.manifest_path)
            .args([
                "--package",
                &project.package_name,
                "--profile",
                "artifact-location",
                "--target-dir",
            ])
            .arg(root.join("build-output"));
        if selected {
            command.args(["--features", "selected"]);
        }
        let built =
            run_cargo_build(&mut command, &project, std::env::consts::DLL_EXTENSION).unwrap();
        assert!(built.path.is_file());
        assert!(
            built
                .path
                .starts_with(root.join("build-output/artifact-location"))
        );
        assert_eq!(
            built.features,
            if selected { vec!["selected"] } else { vec![] }
        );
        let digest = xlfn_package::sha256(&project.lockfile_path).unwrap();
        assert_ne!(digest, old_digest);
        assert_eq!(built.lockfile_sha256.as_deref(), Some(digest.as_str()));
    }
}
