use super::*;

#[derive(Clone, Debug)]
pub struct VerifiedArtifact {
    pub(crate) relative_path: PathBuf,
    pub(crate) bytes: Arc<[u8]>,
    pub(crate) size: u64,
    pub(crate) sha256: [u8; 32],
    pub(crate) permissions: std::fs::Permissions,
}

impl VerifiedArtifact {
    #[must_use]
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    #[must_use]
    pub fn sha256_hex(&self) -> String {
        digest_hex(&self.sha256)
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedPackage {
    pub(crate) artifacts: Vec<VerifiedArtifact>,
    pub(crate) expected_names: BTreeSet<String>,
}

impl VerifiedPackage {
    #[must_use]
    pub fn artifacts(&self) -> &[VerifiedArtifact] {
        &self.artifacts
    }

    /// Adds the serialized build manifest to the closed-world package.
    ///
    /// The manifest is deliberately part of the package's verified artifact
    /// set. A package cannot become commit-ready until its manifest describes
    /// the exact non-manifest artifacts that will be installed.
    pub fn with_manifest_bytes(mut self, bytes: Vec<u8>) -> PackageResult<Self> {
        let relative_path = PathBuf::from("build-manifest.json");
        let name_key = windows_name_key("build manifest", "build-manifest.json")?;
        if !self.expected_names.insert(name_key) {
            return Err("package already contains build-manifest.json".into());
        }
        let artifact = verified_artifact(relative_path, Arc::from(bytes), manifest_permissions()?);
        self.artifacts.push(artifact);
        validate_manifest_bytes(&self.artifacts)
            .map(|()| self)
            .map_err(|error| PackageError::InvalidBuildManifest(error.to_string()))
    }

    /// Prepares a closed-world commit guard for a fully materialized target
    /// directory. The guard owns the directory identity, every opened file
    /// identity, and the expected entry set used by the transaction.
    pub fn prepare_commit(
        &self,
        staging: &Path,
        target: &str,
    ) -> PackageResult<PreparedPackageCommit> {
        if self.artifacts.len() > MAX_PREPARED_ENTRIES {
            return Err(
                format!("package contains more than {MAX_PREPARED_ENTRIES} artifacts").into(),
            );
        }
        let total_bytes = self.artifacts.iter().try_fold(0_u64, |total, artifact| {
            total
                .checked_add(artifact.size)
                .ok_or_else(|| "package artifact byte budget overflow".to_owned())
        })?;
        if total_bytes > MAX_PREPARED_BYTES {
            return Err(
                format!("package contains more than {MAX_PREPARED_BYTES} artifact bytes").into(),
            );
        }
        let (staging_identity, actual_names) =
            inspect_directory_identity(staging, MAX_PREPARED_ENTRIES)?;
        ensure_exact_entry_set(&actual_names, &self.expected_names, staging)?;

        let mut entries = Vec::with_capacity(self.artifacts.len());
        for artifact in &self.artifacts {
            let path = staging.join(&artifact.relative_path);
            let identity = verify_staged_artifact(target, &path, artifact, ExpectedIdentity::Any)?;
            entries.push(PreparedArtifact {
                artifact: artifact.clone(),
                identity,
            });
        }
        ensure_directory_identity(staging, staging_identity, target)?;
        validate_manifest_bytes(&self.artifacts)
            .map_err(|error| PackageError::InvalidBuildManifest(error.to_string()))?;

        Ok(PreparedPackageCommit {
            staging_directory: staging.to_path_buf(),
            staging_directory_identity: staging_identity,
            entries,
            expected_names: self.expected_names.clone(),
            target: target.to_owned(),
        })
    }

    /// Rebuilds a fresh commit directory from the verified artifact bytes.
    pub fn materialize(&self, destination: &PrivateStagingDirectory) -> PackageResult {
        destination.verify()?;
        let destination_path = destination.path();
        let canonical_destination = fs::canonicalize(destination_path)?;
        let mut installed_identities = Vec::with_capacity(self.artifacts.len());

        for artifact in &self.artifacts {
            destination.verify()?;
            let relative = &artifact.relative_path;
            if relative.components().count() != 1
                || !matches!(relative.components().next(), Some(Component::Normal(_)))
            {
                return Err(format!(
                    "verified artifact is not a basename: {}",
                    relative.display()
                )
                .into());
            }
            let output = destination_path.join(relative);
            reject_reparse_points(&canonical_destination, &relative.to_string_lossy())?;
            destination.verify()?;
            let mut temp_file = tempfile::Builder::new()
                .prefix(".verified-file-")
                .tempfile_in(destination_path)?;
            let canonical_temp = fs::canonicalize(temp_file.path())?;
            if !path_is_within(&canonical_destination, &canonical_temp) {
                return Err(format!(
                    "materialized file escapes destination directory: {} vs {}",
                    canonical_temp.display(),
                    canonical_destination.display()
                )
                .into());
            }
            temp_file.as_file_mut().write_all(&artifact.bytes)?;
            temp_file.as_file_mut().flush()?;
            temp_file
                .as_file_mut()
                .set_permissions(artifact.permissions.clone())?;
            temp_file.as_file_mut().sync_all()?;
            temp_file.persist_noclobber(&output).map_err(|error| {
                PackageError::Message(format!(
                    "failed to place verified artifact {}: {error}",
                    output.display()
                ))
            })?;
            destination.verify()?;
            let canonical_output = fs::canonicalize(&output)?;
            if !path_is_within(&canonical_destination, &canonical_output) {
                return Err(format!(
                    "materialized output escapes destination directory: {} vs {}",
                    canonical_output.display(),
                    canonical_destination.display()
                )
                .into());
            }

            let output_file = open_staged_file_no_follow(&output)?;
            let output_metadata = output_file.metadata()?;
            if !output_metadata.is_file() || is_reparse_point(&output_metadata) {
                return Err(format!(
                    "materialized artifact is not a regular file: {}",
                    output.display()
                )
                .into());
            }
            installed_identities.push(file_snapshot_state(&output_file)?.identity);
        }

        destination.verify()?;
        let (identity, actual_names) =
            inspect_directory_identity(destination_path, MAX_PREPARED_ENTRIES)?;
        if identity != destination.identity {
            return Err(format!(
                "materialized directory identity changed: {}",
                destination_path.display()
            )
            .into());
        }
        ensure_exact_entry_set(&actual_names, &self.expected_names, destination_path)?;
        for (artifact, expected_identity) in self.artifacts.iter().zip(installed_identities) {
            verify_staged_artifact(
                "materialization",
                &destination_path.join(&artifact.relative_path),
                artifact,
                ExpectedIdentity::Exact(expected_identity),
            )?;
        }
        ensure_directory_identity(destination_path, destination.identity, "materialization")?;
        Ok(())
    }
}
pub fn verify_staged_package(
    xll: &Path,
    target: &str,
    required_exports: &[String],
    mut bundle: StagedBundle,
) -> PackageResult<VerifiedPackage> {
    let xll_snapshot = snapshot_staged_artifact(target, xll)?;
    verify_xll_bytes(&xll_snapshot, target, required_exports, xll)?;
    for file in &mut bundle.files {
        let expected = file.snapshot.as_deref().ok_or_else(|| {
            PackageError::Message(format!(
                "staged bundle file has no immutable snapshot: {}",
                file.source.display()
            ))
        })?;
        let current = snapshot_staged_artifact(target, &file.source)?;
        if current.as_ref() != expected {
            return Err(PackageError::StagedArtifactChanged {
                target: target.to_owned(),
                path: file.source.clone(),
            });
        }
        file.snapshot = Some(current);
    }
    verify_dependency_closure(xll, target, &bundle, &xll_snapshot)?;

    let mut relative_paths = BTreeSet::new();
    let mut expected_names = BTreeSet::new();
    let mut artifacts = Vec::with_capacity(bundle.files.len() + 1);
    let xll_relative_path = artifact_relative_path(xll, "XLL")?;
    let xll_name = xll_relative_path
        .to_str()
        .ok_or_else(|| format!("XLL basename is not valid UTF-8: {}", xll.display()))?;
    let xll_name_key = windows_name_key("XLL", xll_name)?;
    if !relative_paths.insert(xll_name_key.clone()) {
        return Err(format!("duplicate staged artifact basename: {}", xll.display()).into());
    }
    expected_names.insert(xll_name_key);
    let xll_permissions = staged_artifact_permissions(target, xll)?;
    artifacts.push(verified_artifact(
        xll_relative_path,
        xll_snapshot,
        xll_permissions,
    ));
    for file in bundle.files {
        let relative_path = PathBuf::from(&file.name);
        let name_key = windows_name_key("bundle file", &file.name)?;
        if !relative_paths.insert(name_key.clone()) {
            return Err(format!("duplicate staged artifact basename: {}", file.name).into());
        }
        expected_names.insert(name_key);
        let snapshot = file.snapshot.ok_or_else(|| {
            PackageError::Message(format!(
                "staged bundle file has no immutable snapshot: {}",
                file.source.display()
            ))
        })?;
        artifacts.push(verified_artifact(relative_path, snapshot, file.permissions));
    }
    Ok(VerifiedPackage {
        artifacts,
        expected_names,
    })
}
