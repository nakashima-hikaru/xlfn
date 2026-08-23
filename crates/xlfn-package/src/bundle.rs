use super::*;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleMetadata {
    #[serde(default)]
    pub x86: Vec<String>,
    #[serde(default)]
    pub x64: Vec<String>,
    #[serde(default, rename = "external-imports")]
    pub external_imports: Vec<String>,
    #[serde(default = "default_strict_paths", rename = "strict-paths")]
    pub strict_paths: bool,
}

pub(crate) fn default_strict_paths() -> bool {
    true
}

impl Default for BundleMetadata {
    fn default() -> Self {
        Self {
            x86: Vec::new(),
            x64: Vec::new(),
            external_imports: Vec::new(),
            strict_paths: default_strict_paths(),
        }
    }
}
#[derive(Clone)]
pub(crate) struct BundleFile {
    pub(crate) source: PathBuf,
    pub(crate) name: String,
    pub(crate) configured_path: String,
    pub(crate) snapshot: Option<Arc<[u8]>>,
    pub(crate) permissions: std::fs::Permissions,
}

impl std::fmt::Debug for BundleFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BundleFile")
            .field("source", &self.source)
            .field("name", &self.name)
            .field("configured_path", &self.configured_path)
            .field(
                "snapshot_len",
                &self.snapshot.as_deref().map(|snapshot| snapshot.len()),
            )
            .field("permissions", &self.permissions)
            .finish()
    }
}

#[derive(Debug)]
pub struct ResolvedBundle {
    pub(crate) files: Vec<BundleFile>,
    pub(crate) external_imports: BTreeSet<String>,
}

impl ResolvedBundle {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            files: Vec::new(),
            external_imports: BTreeSet::new(),
        }
    }

    pub fn resolved_files(&self) -> impl Iterator<Item = (&str, &Path)> {
        self.files
            .iter()
            .map(|file| (file.configured_path.as_str(), file.source.as_path()))
    }

    pub fn external_imports(&self) -> impl Iterator<Item = &str> {
        self.external_imports.iter().map(String::as_str)
    }
}

#[derive(Clone, Debug)]
pub struct StagedBundle {
    pub(crate) files: Vec<BundleFile>,
    pub(crate) external_imports: BTreeSet<String>,
}

impl StagedBundle {
    pub fn try_add_external_imports(
        &mut self,
        imports: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> PackageResult {
        let normalized = imports
            .into_iter()
            .map(|name| windows_dll_name_key("external import", name.as_ref()))
            .collect::<PackageResult<Vec<_>>>()?;
        self.external_imports.extend(normalized);
        Ok(())
    }
}

pub fn resolve_bundle_files(
    manifest_directory: &Path,
    target: &str,
    configured: &[String],
) -> PackageResult<ResolvedBundle> {
    resolve_bundle_files_with_policy(manifest_directory, target, configured, &[], true)
}

pub fn resolve_bundle_files_with_metadata(
    manifest_directory: &Path,
    target: &str,
    metadata: &BundleMetadata,
) -> PackageResult<ResolvedBundle> {
    let configured = match Architecture::parse(target)? {
        Architecture::X86 => &metadata.x86,
        Architecture::X64 => &metadata.x64,
    };
    resolve_bundle_files_with_policy(
        manifest_directory,
        target,
        configured,
        &metadata.external_imports,
        metadata.strict_paths,
    )
}

pub fn resolve_bundle_files_with_policy(
    manifest_directory: &Path,
    target: &str,
    configured: &[String],
    external_imports: &[String],
    strict_paths: bool,
) -> PackageResult<ResolvedBundle> {
    resolve_bundle_files_with_policy_impl(
        manifest_directory,
        target,
        configured,
        external_imports,
        strict_paths,
        &NoopSnapshotObserver,
    )
}

pub(crate) fn resolve_bundle_files_with_policy_impl(
    manifest_directory: &Path,
    target: &str,
    configured: &[String],
    external_imports: &[String],
    strict_paths: bool,
    observer: &impl SnapshotObserver,
) -> PackageResult<ResolvedBundle> {
    Architecture::parse(target)?;
    let canonical_root = fs::canonicalize(manifest_directory).map_err(|error| {
        PackageError::Message(format!(
            "failed to resolve manifest directory {}: {error}",
            manifest_directory.display()
        ))
    })?;
    let external_imports = normalize_external_imports(external_imports)?;
    let mut names = BTreeSet::new();
    let mut files = Vec::with_capacity(configured.len());
    for configured_path in configured {
        validate_relative("bundle file", configured_path)?;
        if strict_paths {
            reject_reparse_points(&canonical_root, configured_path)?;
        }
        let unresolved_source = canonical_root.join(configured_path);
        let mut opened_source = open_bundle_source_for_snapshot(&unresolved_source)
            .map_err(|error| map_snapshot_open_error(target, &unresolved_source, error))?;
        observer.after_open(&unresolved_source);
        let source = fs::canonicalize(&unresolved_source).map_err(|error| {
            PackageError::Message(format!(
                "{target}: failed to resolve {}: {error}",
                unresolved_source.display()
            ))
        })?;
        if !path_is_within(&canonical_root, &source) {
            return Err(format!(
                "{target}: bundle file escapes manifest directory: {} resolves to {}",
                unresolved_source.display(),
                source.display()
            )
            .into());
        }
        let opened_metadata = opened_source.metadata()?;
        if !opened_metadata.is_file() {
            return Err(format!("{target}: missing {}", source.display()).into());
        }
        let canonical_source = open_bundle_source_for_snapshot(&source)
            .map_err(|error| map_snapshot_open_error(target, &source, error))?;
        if !same_file_identity(&opened_source, &canonical_source)? {
            return Err(unstable_bundle_source(target, &unresolved_source));
        }
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("bundle file has no UTF-8 basename: {}", source.display()))?
            .to_owned();
        let name_key = windows_name_key("bundle file", &name)?;
        if is_system(&name_key) {
            return Err(format!("bundle must not shadow Windows system DLL {name:?}").into());
        }
        if !names.insert(name_key) {
            return Err(format!("duplicate bundle basename {name:?}").into());
        }
        let snapshot = read_stable_snapshot(target, &source, &mut opened_source, observer)?;
        #[cfg(not(target_os = "windows"))]
        verify_snapshot_against_second_read(target, &source, &mut opened_source, &snapshot)?;
        files.push(BundleFile {
            source,
            name,
            configured_path: configured_path.clone(),
            snapshot: Some(snapshot),
            permissions: opened_metadata.permissions(),
        });
    }
    Ok(ResolvedBundle {
        files,
        external_imports,
    })
}

pub fn verify_bundle_files(bundle: &ResolvedBundle, target: &str) -> PackageResult {
    validate_imports(
        &bundle.files,
        Architecture::parse(target)?,
        &bundle.external_imports,
    )
}

/// Opens a packaging source with the same stable-snapshot guarantees used for
/// bundled DLLs and returns the bytes read from that fixed file identity.
pub fn snapshot_file(target: &str, path: &Path) -> PackageResult<Arc<[u8]>> {
    let mut file = open_bundle_source_for_snapshot(path)
        .map_err(|error| map_snapshot_open_error(target, path, error))?;
    if !file.metadata()?.is_file() {
        return Err(format!("{target}: source is not a file: {}", path.display()).into());
    }
    let snapshot = read_stable_snapshot(target, path, &mut file, &NoopSnapshotObserver)?;
    #[cfg(not(target_os = "windows"))]
    verify_snapshot_against_second_read(target, path, &mut file, &snapshot)?;
    Ok(snapshot)
}

pub fn stage_bundle(
    bundle: &ResolvedBundle,
    destination: &PrivateStagingDirectory,
) -> PackageResult<StagedBundle> {
    destination.verify()?;
    let destination_path = destination.path();
    let canonical_destination = fs::canonicalize(destination_path)?;

    let mut files = Vec::with_capacity(bundle.files.len());
    for file in &bundle.files {
        reject_reparse_points(&canonical_destination, &file.name)?;
        let output = destination_path.join(&file.name);
        if fs::symlink_metadata(&output).is_ok() {
            return Err(format!(
                "destination file already exists or is symlink: {}",
                output.display()
            )
            .into());
        }

        let mut temp_file = tempfile::Builder::new()
            .prefix(".stage-file-")
            .tempfile_in(destination_path)?;

        let temp_path = temp_file.path();
        let canonical_temp = fs::canonicalize(temp_path)?;
        if !path_is_within(&canonical_destination, &canonical_temp) {
            return Err(format!(
                "staging file escapes destination directory: {} vs {}",
                canonical_temp.display(),
                canonical_destination.display()
            )
            .into());
        }

        let snapshot = file.snapshot.clone().ok_or_else(|| {
            PackageError::Message(format!(
                "resolved bundle file has no immutable snapshot: {}",
                file.source.display()
            ))
        })?;
        temp_file.as_file_mut().write_all(snapshot.as_ref())?;
        temp_file.as_file_mut().flush()?;
        temp_file
            .as_file_mut()
            .set_permissions(file.permissions.clone())?;

        temp_file.as_file_mut().sync_all()?;
        temp_file.persist_noclobber(&output).map_err(|err| {
            PackageError::Message(format!(
                "failed to place staged file {}: {err}",
                output.display()
            ))
        })?;

        let canonical_output = fs::canonicalize(&output)?;
        if !path_is_within(&canonical_destination, &canonical_output) {
            return Err(format!(
                "staged output file escapes destination directory: {} vs {}",
                canonical_output.display(),
                canonical_destination.display()
            )
            .into());
        }

        files.push(BundleFile {
            source: output,
            name: file.name.clone(),
            configured_path: file.configured_path.clone(),
            snapshot: Some(Arc::clone(&snapshot)),
            permissions: file.permissions.clone(),
        });
    }

    destination.verify()?;
    let (identity, actual_names) =
        inspect_directory_identity(destination_path, MAX_PREPARED_ENTRIES)?;
    if identity != destination.identity {
        return Err(format!(
            "staged bundle directory identity changed: {}",
            destination_path.display()
        )
        .into());
    }
    let expected_names = files
        .iter()
        .map(|file| windows_name_key("staged bundle entry", &file.name))
        .collect::<PackageResult<BTreeSet<_>>>()?;
    ensure_exact_entry_set(&actual_names, &expected_names, destination_path)?;
    for file in &files {
        let path = destination_path.join(&file.name);
        let handle = open_staged_file_no_follow(&path)?;
        let metadata = handle.metadata()?;
        if !metadata.is_file() || is_reparse_point(&metadata) {
            return Err(format!(
                "staged bundle entry is not a regular file: {}",
                path.display()
            )
            .into());
        }
        let snapshot = file.snapshot.as_ref().ok_or_else(|| {
            PackageError::Message(format!(
                "staged bundle entry has no immutable snapshot: {}",
                path.display()
            ))
        })?;
        let artifact = verified_artifact(
            PathBuf::from(&file.name),
            Arc::clone(snapshot),
            file.permissions.clone(),
        );
        let identity = file_snapshot_state(&handle)?.identity;
        verify_staged_artifact(
            "bundle staging",
            &path,
            &artifact,
            ExpectedIdentity::Exact(identity),
        )?;
    }
    ensure_directory_identity(destination_path, destination.identity, "bundle staging")?;
    Ok(StagedBundle {
        files,
        external_imports: bundle.external_imports.clone(),
    })
}
