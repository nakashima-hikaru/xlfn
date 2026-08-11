use super::*;

pub(crate) fn validate_manifest_bytes(artifacts: &[VerifiedArtifact]) -> PackageResult {
    let manifest = artifacts
        .iter()
        .find(|artifact| artifact.relative_path == Path::new("build-manifest.json"))
        .ok_or_else(|| PackageError::InvalidBuildManifest("manifest artifact is missing".into()))?;
    let value: Value = serde_json::from_slice(&manifest.bytes)?;
    let files = value
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| PackageError::InvalidBuildManifest("files must be an array".into()))?;
    let mut described = BTreeMap::new();
    for file in files {
        let object = file.as_object().ok_or_else(|| {
            PackageError::InvalidBuildManifest("files entries must be objects".into())
        })?;
        let relative_path = object
            .get("relative_path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                PackageError::InvalidBuildManifest(
                    "manifest file entry is missing relative_path".into(),
                )
            })?;
        let size = object.get("size").and_then(Value::as_u64).ok_or_else(|| {
            PackageError::InvalidBuildManifest("manifest file entry is missing numeric size".into())
        })?;
        let sha256 = object
            .get("sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                PackageError::InvalidBuildManifest("manifest file entry is missing sha256".into())
            })?;
        if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(PackageError::InvalidBuildManifest(format!(
                "invalid sha256 for {relative_path:?}"
            )));
        }
        let key = windows_name_key("manifest relative_path", relative_path)?;
        if key == "build-manifest.json"
            || described
                .insert(key, (relative_path, size, sha256))
                .is_some()
        {
            return Err(PackageError::InvalidBuildManifest(format!(
                "duplicate or self-referential manifest entry {relative_path:?}"
            )));
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
