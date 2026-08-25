use super::*;

pub(crate) fn built_library_path(
    metadata: &ProjectMetadata,
    target: &str,
    build: &BuildSelectionArgs,
    default_profile: Option<&str>,
    target_directory: &Path,
) -> PathBuf {
    let profile = build
        .profile
        .as_deref()
        .or(default_profile)
        .unwrap_or("dev");
    let profile_directory = if profile == "dev" { "debug" } else { profile };
    target_directory
        .to_path_buf()
        .join(target)
        .join(profile_directory)
        .join(format!("{}.dll", metadata.lib_name.replace('-', "_")))
}

pub(crate) fn validate_bundle_output_names(
    bundle: &xlfn_package::ResolvedBundle,
    artifact_name: &str,
) -> Result {
    for (configured_path, source) in bundle.resolved_files() {
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .context("bundle file basename is not valid UTF-8")?;
        if is_reserved_distribution_name(name, artifact_name) {
            bail!("bundle file {configured_path:?} uses reserved distribution basename {name:?}");
        }
    }
    Ok(())
}

pub(crate) fn is_reserved_distribution_name(name: &str, artifact_name: &str) -> bool {
    name.eq_ignore_ascii_case(&format!("{artifact_name}.xll"))
        || name.eq_ignore_ascii_case("build-manifest.json")
}
