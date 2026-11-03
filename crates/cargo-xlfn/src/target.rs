use super::*;

pub(crate) fn validate_bundle_output_names(
    bundle: &xlfn_package::ResolvedBundle,
    artifact_name: &str,
) -> Result {
    for file in bundle.resolved_files() {
        let name = file.staged_name;
        if is_reserved_distribution_name(name, artifact_name) {
            bail!(
                "bundle file {:?} uses reserved distribution basename {name:?}",
                file.configured_path
            );
        }
    }
    Ok(())
}

pub(crate) fn is_reserved_distribution_name(name: &str, artifact_name: &str) -> bool {
    name.eq_ignore_ascii_case(&format!("{artifact_name}.xll"))
        || name.eq_ignore_ascii_case("build-manifest.json")
}
