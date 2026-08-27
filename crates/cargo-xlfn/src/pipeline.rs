use super::*;
use std::sync::Arc;

/// The profile a command uses when the user does not select one explicitly.
/// Check and package intentionally keep different defaults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DefaultBuildProfile {
    Dev,
    Release,
}

impl DefaultBuildProfile {
    const fn name(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Release => "release",
        }
    }

    const fn cargo_default(self) -> Option<&'static str> {
        match self {
            // Preserve Cargo's native default for dev builds.
            Self::Dev => None,
            Self::Release => Some("release"),
        }
    }
}

/// One resolved profile shared by Cargo invocation, output lookup, and the
/// build manifest. Keeping these values together prevents those operations
/// from observing different profiles for the same target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedBuildProfile {
    name: String,
    cargo_profile: Option<String>,
    output_directory: String,
}

impl ResolvedBuildProfile {
    fn resolve(build: &BuildSelectionArgs, default: DefaultBuildProfile) -> Self {
        let (name, cargo_profile) = match build.profile.as_deref() {
            Some(profile) => (profile.to_owned(), Some(profile.to_owned())),
            None => (
                default.name().to_owned(),
                default.cargo_default().map(str::to_owned),
            ),
        };
        let output_directory = if name == "dev" {
            "debug".to_owned()
        } else {
            name.clone()
        };
        Self {
            name,
            cargo_profile,
            output_directory,
        }
    }

    fn cargo_profile(&self) -> Option<&str> {
        self.cargo_profile.as_deref()
    }

    pub(crate) fn output_directory(&self) -> &str {
        &self.output_directory
    }
}

/// Inputs needed to build and verify one Windows target. The request carries
/// command-independent build policy; the pipeline owns the artifact policy.
pub(crate) struct TargetBuildRequest<'a> {
    pub(crate) target: WindowsTarget,
    pub(crate) metadata: &'a ProjectMetadata,
    pub(crate) build: &'a BuildSelectionArgs,
    pub(crate) default_profile: DefaultBuildProfile,
    pub(crate) target_directory: &'a Path,
}

/// Bundle resolution and the metadata derived from that same resolution.
/// Verification and manifest generation consume this single context.
struct ResolvedTargetBundle {
    bundle: xlfn_package::ResolvedBundle,
    strict_paths: bool,
    bundle_sources: Vec<xlfn_package::BundleSource>,
    declared_external_imports: Vec<String>,
}

impl ResolvedTargetBundle {
    fn resolve(metadata: &ProjectMetadata, target: WindowsTarget) -> Result<Self> {
        let (bundle, strict_paths) = match &metadata.bundle {
            Some(bundle_metadata) => (
                xlfn_package::resolve_bundle_files_with_metadata(
                    &metadata.manifest_directory,
                    target.triple(),
                    bundle_metadata,
                )?,
                bundle_metadata.strict_paths,
            ),
            None => (xlfn_package::ResolvedBundle::empty(), true),
        };
        validate_bundle_output_names(&bundle, &metadata.artifact_name)?;
        let bundle_sources = bundle
            .resolved_files()
            .map(
                |(configured_path, staged_source)| -> Result<xlfn_package::BundleSource> {
                    let staged_relative_path = staged_source
                        .file_name()
                        .and_then(|name| name.to_str())
                        .context("bundle file basename is not valid UTF-8")?;
                    Ok(xlfn_package::BundleSource {
                        configured_path: configured_path.to_owned(),
                        staged_relative_path: staged_relative_path.to_owned(),
                    })
                },
            )
            .collect::<Result<Vec<_>>>()?;
        let declared_external_imports = bundle
            .external_imports()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        Ok(Self {
            bundle,
            strict_paths,
            bundle_sources,
            declared_external_imports,
        })
    }
}

/// Build, snapshot, verify, and assemble the complete artifact set for one
/// target. The returned package is ready for either check (discard) or
/// package (materialize and commit); neither caller reimplements artifact
/// policy.
pub(crate) fn build_and_verify_target(
    request: TargetBuildRequest<'_>,
) -> Result<xlfn_package::VerifiedPackage> {
    let TargetBuildRequest {
        target,
        metadata,
        build,
        default_profile,
        target_directory,
    } = request;
    let profile = ResolvedBuildProfile::resolve(build, default_profile);

    fs::create_dir_all(target_directory)?;
    build_target(target, metadata, build, &profile, target_directory)?;

    let source = built_library_path(metadata, target.triple(), &profile, target_directory);
    if !source.is_file() {
        bail!("built XLL DLL was not found at {}", source.display());
    }
    let source_snapshot = xlfn_package::snapshot_file(target.triple(), &source)?;
    let bundle = ResolvedTargetBundle::resolve(metadata, target)?;

    let verification_guard = tempfile::Builder::new()
        .prefix(".cargo-xlfn-verify-")
        .tempdir_in(target_directory)?;
    let verification_staging =
        xlfn_package::PrivateStagingDirectory::create(&verification_guard.path().join("package"))?;
    let (verified, observation) = verify_target_snapshot(
        target,
        metadata,
        source_snapshot,
        &bundle,
        &verification_staging,
    )?;
    let manifest = build_manifest_input(metadata, build, target, &profile, &bundle, observation);
    Ok(verified.with_build_manifest(manifest)?)
}

fn build_target(
    target: WindowsTarget,
    metadata: &ProjectMetadata,
    build: &BuildSelectionArgs,
    profile: &ResolvedBuildProfile,
    target_directory: &Path,
) -> Result {
    let mut command = cargo_command();
    command
        .args(["build", "--manifest-path"])
        .arg(&metadata.manifest_path)
        .args(["--package", &metadata.package_name])
        .args(["--target", target.triple()]);
    configure_build(&mut command, metadata, target.triple(), target_directory)?;
    build.apply_to_command(&mut command, profile.cargo_profile());
    if !command.status()?.success() {
        bail!("cargo build failed for {}", target.triple());
    }
    Ok(())
}

fn verify_target_snapshot(
    target: WindowsTarget,
    metadata: &ProjectMetadata,
    source_snapshot: Arc<[u8]>,
    resolved_bundle: &ResolvedTargetBundle,
    staging: &xlfn_package::PrivateStagingDirectory,
) -> Result<(xlfn_package::VerifiedPackage, CrtObservation)> {
    let mut staged_bundle = xlfn_package::stage_bundle(&resolved_bundle.bundle, staging)?;
    let xll = staging
        .path()
        .join(format!("{}.xll", metadata.artifact_name));
    if fs::symlink_metadata(&xll).is_ok() {
        bail!(
            "bundle basename collides with generated XLL: {}",
            xll.display()
        );
    }
    fs::write(&xll, source_snapshot.as_ref())?;

    let observation = CrtObservation::inspect(&xlfn_package::inspect_pe(&xll)?, metadata.crt)?;
    observation.warn_if_mixed();
    // Dynamic MSVC runtimes are redistributable external dependencies, not
    // Windows inbox DLLs. Admit only names observed in this exact XLL image.
    staged_bundle.try_add_external_imports(&observation.observed_dynamic_crt_imports)?;
    let verified = xlfn_package::verify_staged_package(&xll, target.triple(), &[], staged_bundle)?;
    Ok((verified, observation))
}

fn build_manifest_input(
    metadata: &ProjectMetadata,
    build: &BuildSelectionArgs,
    target: WindowsTarget,
    profile: &ResolvedBuildProfile,
    bundle: &ResolvedTargetBundle,
    observation: CrtObservation,
) -> xlfn_package::BuildManifestInput {
    xlfn_package::BuildManifestInput {
        package: metadata.package_name.clone(),
        package_version: metadata.package_version.clone(),
        artifact: metadata.artifact_name.clone(),
        target: target.triple().to_owned(),
        profile: profile.name.clone(),
        feature_selection: xlfn_package::FeatureSelection {
            explicit: build.features.clone(),
            default_features: !build.no_default_features,
            all_features: build.all_features,
            resolved: metadata.resolved_features.clone(),
        },
        cargo_constraints: xlfn_package::CargoConstraints {
            locked: build.locked,
            frozen: build.frozen,
            offline: build.offline,
            lockfile_sha256: metadata.lockfile_sha256.clone(),
        },
        crt: observation.manifest(metadata.crt),
        bundle_sources: bundle.bundle_sources.clone(),
        bundle_policy: xlfn_package::BundlePolicy {
            strict_paths: bundle.strict_paths,
            system_import_policy: xlfn_package::SYSTEM_IMPORT_POLICY_VERSION.to_owned(),
            external_imports: bundle.declared_external_imports.clone(),
        },
        integrity: xlfn_package::IntegrityMetadata::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profiles_keep_cargo_and_output_paths_aligned() {
        let build = BuildSelectionArgs::default();
        let dev = ResolvedBuildProfile::resolve(&build, DefaultBuildProfile::Dev);
        assert_eq!(dev.name, "dev");
        assert_eq!(dev.cargo_profile(), None);
        assert_eq!(dev.output_directory(), "debug");

        let release = ResolvedBuildProfile::resolve(&build, DefaultBuildProfile::Release);
        assert_eq!(release.name, "release");
        assert_eq!(release.cargo_profile(), Some("release"));
        assert_eq!(release.output_directory(), "release");
    }

    #[test]
    fn explicit_profile_controls_all_profile_observers() {
        let build = BuildSelectionArgs {
            profile: Some("ci".to_owned()),
            ..BuildSelectionArgs::default()
        };
        let profile = ResolvedBuildProfile::resolve(&build, DefaultBuildProfile::Release);
        assert_eq!(profile.name, "ci");
        assert_eq!(profile.cargo_profile(), Some("ci"));
        assert_eq!(profile.output_directory(), "ci");
    }
}
