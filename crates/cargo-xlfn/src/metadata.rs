use super::*;

pub(crate) struct ProjectMetadata {
    pub(crate) package_name: String,
    pub(crate) package_version: String,
    pub(crate) lib_name: String,
    pub(crate) artifact_name: String,
    pub(crate) manifest_path: PathBuf,
    pub(crate) manifest_directory: PathBuf,
    pub(crate) target_directory: PathBuf,
    pub(crate) crt: ResolvedCrtPolicy,
    pub(crate) resolved_features: Vec<String>,
    pub(crate) lockfile_sha256: Option<String>,
    pub(crate) bundle: Option<BundleMetadata>,
}

pub(crate) fn parse_bundle_metadata(value: serde_json::Value) -> Result<BundleMetadata> {
    serde_path_to_error::deserialize(value).map_err(|error| {
        let path = error.path().to_string();

        if path.is_empty() {
            anyhow!("invalid [package.metadata.xlfn.bundle]: {}", error.inner())
        } else {
            anyhow!(
                "invalid [package.metadata.xlfn.bundle] at {path}: {}",
                error.inner()
            )
        }
    })
}

pub(crate) fn project_metadata(
    args: &ProjectArgs,
    build: &BuildSelectionArgs,
) -> Result<ProjectMetadata> {
    // Discover the selected package without resolving dependencies or applying
    // package-relative feature names. `cargo metadata` has no `--package` option,
    // so an unqualified `--features foo` at a virtual workspace root can select
    // the wrong package or fail before the requested member is known.
    let mut discovery_command = MetadataCommand::new();
    discovery_command.no_deps();
    if let Some(path) = &args.manifest_path {
        discovery_command.manifest_path(path);
    }
    build.apply_resolution_constraints(&mut discovery_command);
    let discovery = discovery_command.exec()?;
    let current_directory = std::env::current_dir()?;
    let selected = select_discovery_package(&discovery, args, &current_directory)?;
    let selected_id = selected.id.clone();
    let selected_manifest = selected.manifest_path.clone();

    // Resolve again from the selected member manifest with exactly the feature
    // and lock/network constraints that will be passed to `cargo build`.
    let mut command = MetadataCommand::new();
    command.manifest_path(selected_manifest.as_std_path());
    build.apply_to_metadata(&mut command);
    let cargo = command.exec()?;
    let package = cargo
        .packages
        .iter()
        .find(|package| package.id == selected_id)
        .context("selected package disappeared from resolved Cargo metadata")?;
    let libraries = package
        .targets
        .iter()
        .filter(|target| target.kind.iter().any(|kind| kind.to_string() == "cdylib"))
        .collect::<Vec<_>>();
    if libraries.len() != 1 {
        bail!(
            "package {} must have exactly one cdylib target, found {}",
            package.name,
            libraries.len()
        );
    }
    let metadata = package.metadata.get("xlfn");
    let metadata_crt = metadata
        .and_then(|value| value.get("crt"))
        .map(CrtPolicy::parse_metadata)
        .transpose()?;
    let crt = ResolvedCrtPolicy::resolve(build.crt, metadata_crt);
    let artifact_name = metadata
        .and_then(|value| value.get("artifact-name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(package.name.as_str())
        .to_owned();
    validate_windows_basename(&artifact_name)
        .with_context(|| format!("invalid artifact-name {artifact_name:?}"))?;
    let manifest_directory = package
        .manifest_path
        .parent()
        .context("package manifest has no parent")?
        .as_std_path()
        .to_path_buf();
    let bundle = metadata
        .and_then(|value| value.get("bundle"))
        .cloned()
        .map(parse_bundle_metadata)
        .transpose()?;
    let mut resolved_features = cargo
        .resolve
        .as_ref()
        .and_then(|resolve| resolve.nodes.iter().find(|node| node.id == package.id))
        .map(|node| {
            node.features
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    resolved_features.sort_unstable();
    resolved_features.dedup();
    let lockfile = cargo.workspace_root.as_std_path().join("Cargo.lock");
    let lockfile_sha256 = lockfile
        .is_file()
        .then(|| xlfn_package::sha256(&lockfile))
        .transpose()?;
    Ok(ProjectMetadata {
        package_name: package.name.to_string(),
        package_version: package.version.to_string(),
        lib_name: libraries[0].name.clone(),
        artifact_name,
        manifest_path: package.manifest_path.as_std_path().to_path_buf(),
        manifest_directory,
        target_directory: build
            .target_dir
            .clone()
            .unwrap_or_else(|| cargo.target_directory.as_std_path().to_path_buf()),
        crt,
        resolved_features,
        lockfile_sha256,
        bundle,
    })
}

pub(crate) fn select_discovery_package<'a>(
    discovery: &'a Metadata,
    args: &ProjectArgs,
    current_directory: &Path,
) -> Result<&'a Package> {
    if let Some(name) = &args.package {
        return discovery
            .packages
            .iter()
            .find(|package| package.name.as_str() == name)
            .ok_or_else(|| anyhow!("workspace has no package {name:?}"));
    }

    if let Some(path) = &args.manifest_path {
        let canonical_target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        return discovery
            .packages
            .iter()
            .find(|package| {
                let package_manifest = package.manifest_path.as_std_path();
                fs::canonicalize(package_manifest)
                    .map(|canonical| canonical == canonical_target)
                    .unwrap_or(false)
                    || package_manifest == path
            })
            .ok_or_else(|| anyhow!("workspace has no package matching manifest path {path:?}"));
    }

    if let Some(package) = package_for_current_directory(discovery, current_directory) {
        return Ok(package);
    }

    if let Some(root) = discovery.root_package() {
        return Ok(root);
    }

    if discovery.workspace_members.len() == 1 {
        let member_id = &discovery.workspace_members[0];
        return discovery
            .packages
            .iter()
            .find(|package| &package.id == member_id)
            .ok_or_else(|| anyhow!("workspace member package disappeared"));
    }

    bail!("select a workspace member with --package or --manifest-path")
}

pub(crate) fn package_for_current_directory<'a>(
    discovery: &'a Metadata,
    current_directory: &Path,
) -> Option<&'a Package> {
    let current_directory = fs::canonicalize(current_directory).ok()?;
    discovery
        .packages
        .iter()
        .filter(|package| discovery.workspace_members.contains(&package.id))
        .filter_map(|package| {
            let package_directory = package.manifest_path.parent()?.as_std_path();
            let package_directory = fs::canonicalize(package_directory).ok()?;
            current_directory
                .starts_with(&package_directory)
                .then_some((package, package_directory.components().count()))
        })
        .max_by_key(|(_, depth)| *depth)
        .map(|(package, _)| package)
}
