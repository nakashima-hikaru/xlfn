use super::*;

/// The schema version written into every build manifest.
pub const BUILD_MANIFEST_SCHEMA: u32 = 6;

/// Package-owned input for constructing a build manifest.
///
/// The caller supplies package/build observations, while the package layer
/// derives the artifact inventory and hashes from the verified artifact set.
#[derive(Clone, Debug, Default)]
pub struct BuildManifestInput {
    pub package: String,
    pub package_version: String,
    pub artifact: String,
    pub target: String,
    pub profile: String,
    pub feature_selection: FeatureSelection,
    pub cargo_constraints: CargoConstraints,
    pub crt: CrtManifest,
    pub bundle_sources: Vec<BundleSource>,
    pub bundle_policy: BundlePolicy,
    pub integrity: IntegrityMetadata,
}

/// Feature selection recorded in a build manifest.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureSelection {
    pub explicit: Vec<String>,
    pub default_features: bool,
    pub all_features: bool,
    pub resolved: Vec<String>,
}

/// Cargo reproducibility constraints recorded in a build manifest.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CargoConstraints {
    pub locked: bool,
    pub frozen: bool,
    pub offline: bool,
    pub lockfile_sha256: Option<String>,
}

/// CRT observation recorded in a build manifest.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CrtManifest {
    pub requested: String,
    pub source: String,
    pub effective_rust: String,
    pub enforcement: String,
    pub observed_dynamic_crt_imports: Vec<String>,
    pub consistency: String,
}

/// A configured bundle source and the basename staged into the package.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleSource {
    pub configured_path: String,
    pub staged_relative_path: String,
}

/// Bundle policy recorded in a build manifest.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundlePolicy {
    pub strict_paths: bool,
    pub system_import_policy: String,
    pub external_imports: Vec<String>,
}

/// Integrity metadata recorded in a build manifest.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrityMetadata {
    pub purpose: String,
    pub runtime_verified: bool,
    pub trust_boundary: String,
}

impl Default for IntegrityMetadata {
    fn default() -> Self {
        Self {
            purpose: "audit-metadata-only".to_owned(),
            runtime_verified: false,
            trust_boundary: "protected-install-location-and-native-code-signing".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    relative_path: String,
    size: u64,
    sha256: String,
}

/// A fully assembled, package-owned build manifest.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildManifest {
    schema: u32,
    package: String,
    package_version: String,
    artifact: String,
    target: String,
    profile: String,
    feature_selection: FeatureSelection,
    cargo_constraints: CargoConstraints,
    crt: CrtManifest,
    bundle_sources: Vec<BundleSource>,
    bundle_policy: BundlePolicy,
    integrity: IntegrityMetadata,
    files: Vec<ManifestFile>,
}

impl BuildManifest {
    /// Builds the manifest from caller-supplied observations and verified
    /// artifacts. File names, sizes, and hashes are never caller-controlled.
    pub fn from_input(
        input: BuildManifestInput,
        artifacts: &[VerifiedArtifact],
    ) -> PackageResult<Self> {
        let files = artifacts
            .iter()
            .map(|artifact| {
                let relative_path = artifact.relative_path.to_str().ok_or_else(|| {
                    PackageError::InvalidBuildManifest(format!(
                        "artifact path is not UTF-8: {}",
                        artifact.relative_path.display()
                    ))
                })?;
                if relative_path.eq_ignore_ascii_case("build-manifest.json") {
                    return Err(PackageError::InvalidBuildManifest(
                        "build manifest cannot describe itself".into(),
                    ));
                }
                Ok(ManifestFile {
                    relative_path: relative_path.to_owned(),
                    size: artifact.size,
                    sha256: artifact.sha256_hex(),
                })
            })
            .collect::<PackageResult<Vec<_>>>()?;
        Ok(Self {
            schema: BUILD_MANIFEST_SCHEMA,
            package: input.package,
            package_version: input.package_version,
            artifact: input.artifact,
            target: input.target,
            profile: input.profile,
            feature_selection: input.feature_selection,
            cargo_constraints: input.cargo_constraints,
            crt: input.crt,
            bundle_sources: input.bundle_sources,
            bundle_policy: input.bundle_policy,
            integrity: input.integrity,
            files,
        })
    }

    /// Serializes the validated schema for inclusion in a package.
    pub fn to_bytes(&self) -> PackageResult<Vec<u8>> {
        serde_json::to_vec_pretty(self).map_err(PackageError::from)
    }
}

pub(crate) fn validate_manifest_bytes(artifacts: &[VerifiedArtifact]) -> PackageResult {
    let manifest = artifacts
        .iter()
        .find(|artifact| artifact.relative_path == Path::new("build-manifest.json"))
        .ok_or_else(|| PackageError::InvalidBuildManifest("manifest artifact is missing".into()))?;
    let manifest: BuildManifest = serde_json::from_slice(&manifest.bytes).map_err(|error| {
        PackageError::InvalidBuildManifest(format!("manifest schema is invalid: {error}"))
    })?;
    if manifest.schema != BUILD_MANIFEST_SCHEMA {
        return Err(PackageError::InvalidBuildManifest(format!(
            "unsupported build manifest schema {}",
            manifest.schema
        )));
    }
    let mut described = BTreeMap::new();
    for file in manifest.files {
        let key = windows_name_key("manifest relative_path", &file.relative_path)?;
        if key == "build-manifest.json"
            || described
                .insert(key, (file.relative_path, file.size, file.sha256))
                .is_some()
        {
            return Err(PackageError::InvalidBuildManifest(
                "duplicate or self-referential manifest entry".into(),
            ));
        }
    }

    let actual = artifacts
        .iter()
        .filter(|artifact| artifact.relative_path != Path::new("build-manifest.json"))
        .collect::<Vec<_>>();
    if described.len() != actual.len() {
        return Err(PackageError::InvalidBuildManifest(format!(
            "manifest describes {} files but package contains {}",
            described.len(),
            actual.len()
        )));
    }
    for artifact in actual {
        let name = artifact.relative_path.to_str().ok_or_else(|| {
            PackageError::InvalidBuildManifest("artifact path is not UTF-8".into())
        })?;
        let key = windows_name_key("artifact relative_path", name)?;
        let Some((described_name, size, sha256)) = described.get(&key) else {
            return Err(PackageError::InvalidBuildManifest(format!(
                "manifest does not describe {name:?}"
            )));
        };
        if *described_name != name || *size != artifact.size || *sha256 != artifact.sha256_hex() {
            return Err(PackageError::InvalidBuildManifest(format!(
                "manifest metadata does not match {name:?}"
            )));
        }
    }
    Ok(())
}

pub(crate) fn digest_hex(digest: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(digest.len() * 2);
    for &byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

pub(crate) fn verified_artifact(
    relative_path: PathBuf,
    bytes: Arc<[u8]>,
    permissions: std::fs::Permissions,
) -> VerifiedArtifact {
    VerifiedArtifact {
        relative_path,
        size: bytes.len() as u64,
        sha256: sha256_digest(&bytes),
        permissions,
        bytes,
    }
}

pub(crate) fn artifact_relative_path(path: &Path, label: &str) -> PackageResult<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{label} has no UTF-8 basename: {}", path.display()))?;
    windows_name_key(label, name)?;
    Ok(PathBuf::from(name))
}
