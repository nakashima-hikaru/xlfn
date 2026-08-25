use super::*;

#[derive(Clone, Debug)]
pub(crate) struct PreparedArtifact {
    pub(crate) artifact: VerifiedArtifact,
    pub(crate) identity: FileIdentity,
}

#[derive(Clone, Debug)]
pub struct PreparedPackageCommit {
    pub(crate) staging_directory: PathBuf,
    pub(crate) staging_directory_identity: DirectoryIdentity,
    pub(crate) entries: Vec<PreparedArtifact>,
    pub(crate) expected_names: BTreeSet<String>,
    pub(crate) target: String,
}

#[derive(Clone, Debug)]
pub(crate) enum PreparedDirectoryEntry {
    File {
        name: String,
        identity: FileIdentity,
        bytes: Arc<[u8]>,
    },
    Directory {
        name: String,
        contents: PreparedDirectoryContents,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedDirectoryContents {
    pub(crate) identity: DirectoryIdentity,
    pub(crate) entries: Vec<PreparedDirectoryEntry>,
    pub(crate) expected_names: BTreeSet<String>,
}

/// A closed-world snapshot of a directory tree. Every descendant directory
/// and regular file is retained in the snapshot and revalidated at commit
/// time; directory identity alone is never used as proof of unchanged
/// contents.
#[derive(Clone, Debug)]
pub struct PreparedDirectoryCommit {
    pub(crate) staging_directory: PathBuf,
    pub(crate) staging_directory_identity: DirectoryIdentity,
    pub(crate) entries: Vec<PreparedDirectoryEntry>,
    pub(crate) expected_names: BTreeSet<String>,
}

/// Holds read handles for every source file in a prepared tree through its
/// final source verification. On Windows these descendant handles must be
/// released immediately before renaming the non-empty parent directory;
/// Windows rejects that rename while any descendant file handle remains open.
#[must_use = "keep the source lease alive through final source verification"]
#[derive(Debug, Default)]
pub struct CommitSourceLease {
    pub(crate) handles: Vec<std::fs::File>,
}

#[derive(Default)]
pub(crate) struct PreparedBudget {
    pub(crate) entries: usize,
    pub(crate) bytes: u64,
}

impl PreparedBudget {
    pub(crate) fn account_entry(&mut self, path: &Path) -> PackageResult {
        self.entries = self.entries.checked_add(1).ok_or_else(|| {
            format!(
                "prepared directory entry budget overflow at {}",
                path.display()
            )
        })?;
        if self.entries > MAX_PREPARED_ENTRIES {
            return Err(format!(
                "prepared directory contains more than {MAX_PREPARED_ENTRIES} entries"
            )
            .into());
        }
        Ok(())
    }

    pub(crate) fn reserve_bytes(&mut self, path: &Path, size: u64) -> PackageResult {
        self.bytes = self.bytes.checked_add(size).ok_or_else(|| {
            format!(
                "prepared directory byte budget overflow at {}",
                path.display()
            )
        })?;
        if self.bytes > MAX_PREPARED_BYTES {
            return Err(format!(
                "prepared directory contains more than {MAX_PREPARED_BYTES} bytes"
            )
            .into());
        }
        Ok(())
    }

    pub(crate) fn remaining_entries(&self) -> usize {
        MAX_PREPARED_ENTRIES.saturating_sub(self.entries)
    }
}

impl CommitSourceLease {
    pub(crate) fn lock_package(
        mut self,
        root: &Path,
        expected_root: DirectoryIdentity,
        entries: &[PreparedArtifact],
    ) -> PackageResult<Self> {
        ensure_directory_identity(root, expected_root, "directory commit")?;
        for entry in entries {
            self.lock_file(&root.join(&entry.artifact.relative_path), entry.identity)?;
        }
        ensure_directory_identity(root, expected_root, "directory commit")?;
        Ok(self)
    }

    pub(crate) fn lock_directory(
        mut self,
        root: &Path,
        expected_root: DirectoryIdentity,
        entries: &[PreparedDirectoryEntry],
    ) -> PackageResult<Self> {
        lock_prepared_directory_source(&mut self, root, expected_root, entries)?;
        Ok(self)
    }

    pub(crate) fn lock_file(
        &mut self,
        path: &Path,
        expected_identity: FileIdentity,
    ) -> PackageResult {
        let handle =
            open_commit_source_file(path).map_err(|_| staged_changed("directory commit", path))?;
        let metadata = handle
            .metadata()
            .map_err(|_| staged_changed("directory commit", path))?;
        if !metadata.is_file() || is_reparse_point(&metadata) {
            return Err(staged_changed("directory commit", path));
        }
        let identity = file_snapshot_state(&handle)
            .map_err(|_| staged_changed("directory commit", path))?
            .identity;
        if identity != expected_identity {
            return Err(staged_changed("directory commit", path));
        }
        self.handles.push(handle);
        Ok(())
    }
}
impl PreparedPackageCommit {
    #[must_use]
    pub fn staging_directory(&self) -> &Path {
        &self.staging_directory
    }

    /// Revalidates the source directory immediately before its rename.
    pub fn verify_source_contents(&self) -> PackageResult {
        verify_prepared_package_directory(self, &self.staging_directory, true)
    }

    /// Reopens the committed directory and verifies the closed-world package.
    /// File identity is preferred, with byte equality as the fallback for
    /// filesystems that materialize a new identity during replacement.
    pub fn verify_committed_contents(&self, destination: &Path) -> PackageResult {
        verify_prepared_package_directory(self, destination, false)
    }

    /// Acquires the source lease used for final source verification. Windows
    /// callers must release it immediately before renaming the source directory.
    pub fn lock_source_for_commit(&self) -> PackageResult<CommitSourceLease> {
        CommitSourceLease::default().lock_package(
            &self.staging_directory,
            self.staging_directory_identity,
            &self.entries,
        )
    }

    /// Returns the verified snapshots so a containing directory transaction
    /// can retain the same allocations instead of rereading every artifact.
    pub fn shared_artifacts(&self) -> impl Iterator<Item = (PathBuf, Arc<[u8]>)> + '_ {
        self.entries.iter().map(|entry| {
            (
                entry.artifact.relative_path.clone(),
                Arc::clone(&entry.artifact.bytes),
            )
        })
    }
}

impl PreparedDirectoryCommit {
    pub fn prepare(staging: &Path, expected_names: &[&str]) -> PackageResult<Self> {
        Self::prepare_with_shared_artifacts(staging, expected_names, &BTreeMap::new())
    }

    /// Prepares a directory tree while reusing already verified file
    /// snapshots when their relative paths and bytes match.
    pub fn prepare_with_shared_artifacts(
        staging: &Path,
        expected_names: &[&str],
        shared: &BTreeMap<PathBuf, Arc<[u8]>>,
    ) -> PackageResult<Self> {
        let expected_names = expected_name_set(expected_names)?;
        let mut budget = PreparedBudget::default();
        let (staging_directory_identity, actual_names) =
            inspect_directory_identity(staging, budget.remaining_entries())?;
        ensure_exact_entry_set(&actual_names, &expected_names, staging)?;
        if actual_names.len() > budget.remaining_entries() {
            return Err(format!(
                "prepared directory contains more than {MAX_PREPARED_ENTRIES} entries"
            )
            .into());
        }
        let mut entries = Vec::with_capacity(actual_names.len());
        for (name_key, name) in &actual_names {
            let path = staging.join(name);
            let relative = PathBuf::from(name);
            entries.push(prepare_directory_entry(
                name_key,
                name,
                &path,
                &relative,
                shared,
                &mut budget,
            )?);
        }
        ensure_directory_identity(staging, staging_directory_identity, "directory commit")?;
        Ok(Self {
            staging_directory: staging.to_path_buf(),
            staging_directory_identity,
            entries,
            expected_names,
        })
    }

    #[must_use]
    pub fn staging_directory(&self) -> &Path {
        &self.staging_directory
    }

    pub fn verify_source_contents(&self) -> PackageResult {
        verify_prepared_directory(self, &self.staging_directory, true)
    }

    pub fn verify_committed_contents(&self, destination: &Path) -> PackageResult {
        verify_prepared_directory(self, destination, false)
    }

    /// Acquires the source lease used for final source verification. Windows
    /// callers must release it immediately before renaming the source directory.
    pub fn lock_source_for_commit(&self) -> PackageResult<CommitSourceLease> {
        CommitSourceLease::default().lock_directory(
            &self.staging_directory,
            self.staging_directory_identity,
            &self.entries,
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ExpectedIdentity {
    Any,
    Exact(FileIdentity),
    SameOrBytes(FileIdentity),
}

pub(crate) fn snapshot_staged_artifact(target: &str, path: &Path) -> PackageResult<Arc<[u8]>> {
    let mut file = open_staged_file_no_follow(path).map_err(|_| staged_changed(target, path))?;
    let metadata = file.metadata().map_err(|_| staged_changed(target, path))?;
    if !metadata.is_file() || is_reparse_point(&metadata) {
        return Err(staged_changed(target, path));
    }
    let snapshot = read_stable_snapshot(target, path, &mut file, &NoopSnapshotObserver)?;
    #[cfg(unix)]
    verify_snapshot_against_second_read(target, path, &mut file, &snapshot)?;
    Ok(snapshot)
}

pub(crate) fn staged_artifact_permissions(
    target: &str,
    path: &Path,
) -> PackageResult<std::fs::Permissions> {
    let file = open_staged_file_no_follow(path).map_err(|_| staged_changed(target, path))?;
    let metadata = file.metadata().map_err(|_| staged_changed(target, path))?;
    if !metadata.is_file() || is_reparse_point(&metadata) {
        return Err(staged_changed(target, path));
    }
    Ok(metadata.permissions())
}

pub(crate) fn staged_changed(target: &str, path: &Path) -> PackageError {
    PackageError::StagedArtifactChanged {
        target: target.to_owned(),
        path: path.to_owned(),
    }
}

pub(crate) fn verify_staged_artifact(
    target: &str,
    path: &Path,
    expected: &VerifiedArtifact,
    identity: ExpectedIdentity,
) -> PackageResult<FileIdentity> {
    let mut file = open_staged_file_no_follow(path).map_err(|_| staged_changed(target, path))?;
    let metadata = file.metadata().map_err(|_| staged_changed(target, path))?;
    if !metadata.is_file() || is_reparse_point(&metadata) {
        return Err(staged_changed(target, path));
    }
    let state = file_snapshot_state(&file).map_err(|_| staged_changed(target, path))?;
    if let ExpectedIdentity::Exact(expected_identity) = identity
        && state.identity != expected_identity
    {
        return Err(staged_changed(target, path));
    }
    let snapshot = read_stable_snapshot_with_limit(
        target,
        path,
        &mut file,
        &NoopSnapshotObserver,
        Some(expected.size),
    )?;
    #[cfg(unix)]
    verify_snapshot_against_second_read(target, path, &mut file, &snapshot)?;
    let matches = snapshot.len() as u64 == expected.size
        && sha256_digest(&snapshot) == expected.sha256
        && snapshot.as_ref() == expected.bytes.as_ref();
    if !matches {
        return Err(staged_changed(target, path));
    }
    if let ExpectedIdentity::SameOrBytes(expected_identity) = identity
        && state.identity != expected_identity
        && snapshot.as_ref() != expected.bytes.as_ref()
    {
        return Err(staged_changed(target, path));
    }
    Ok(state.identity)
}

pub(crate) fn expected_name_set(names: &[&str]) -> PackageResult<BTreeSet<String>> {
    names
        .iter()
        .map(|name| windows_name_key("expected directory entry", name))
        .collect()
}

pub(crate) fn inspect_directory_identity(
    path: &Path,
    max_entries: usize,
) -> PackageResult<(DirectoryIdentity, BTreeMap<String, String>)> {
    let directory = open_staged_directory_no_follow(path).map_err(|error| {
        PackageError::Message(format!(
            "failed to open staging directory without following links {}: {error}",
            path.display()
        ))
    })?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || is_reparse_point(&metadata) {
        return Err(format!(
            "staging path is not a regular directory: {}",
            path.display()
        )
        .into());
    }
    let identity = DirectoryIdentity::from_file_identity(file_snapshot_state(&directory)?.identity);
    drop(directory);
    Ok((identity, read_directory_entries(path, max_entries)?))
}

pub(crate) fn ensure_directory_identity(
    path: &Path,
    expected: DirectoryIdentity,
    label: &str,
) -> PackageResult {
    let directory = open_staged_directory_no_follow(path).map_err(|error| {
        PackageError::Message(format!(
            "{label}: failed to reopen directory {}: {error}",
            path.display()
        ))
    })?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || is_reparse_point(&metadata) {
        return Err(format!(
            "{label}: directory is not a regular directory: {}",
            path.display()
        )
        .into());
    }
    let identity = DirectoryIdentity::from_file_identity(file_snapshot_state(&directory)?.identity);
    if identity != expected {
        return Err(format!("{label}: directory identity changed: {}", path.display()).into());
    }
    Ok(())
}

pub(crate) fn read_directory_entries(
    path: &Path,
    max_entries: usize,
) -> PackageResult<BTreeMap<String, String>> {
    let mut entries = BTreeMap::new();
    for entry in fs::read_dir(path)? {
        if entries.len() >= max_entries {
            return Err(format!(
                "directory contains more than the permitted entry budget: {}",
                path.display()
            )
            .into());
        }
        let entry = entry?;
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(|| format!("directory entry is not valid UTF-8: {}", path.display()))?
            .to_owned();
        let key = windows_name_key("directory entry", &name)?;
        if entries.insert(key, name.clone()).is_some() {
            return Err(format!(
                "directory contains duplicate Windows name key {name:?}: {}",
                path.display()
            )
            .into());
        }
    }
    Ok(entries)
}

pub(crate) fn ensure_exact_entry_set(
    actual: &BTreeMap<String, String>,
    expected: &BTreeSet<String>,
    directory: &Path,
) -> PackageResult {
    let actual_keys = actual.keys().cloned().collect::<BTreeSet<_>>();
    if &actual_keys != expected {
        return Err(format!(
            "closed-world directory entry mismatch in {}: expected {:?}, found {:?}",
            directory.display(),
            expected,
            actual_keys
        )
        .into());
    }
    Ok(())
}

pub(crate) fn prepare_directory_entry(
    name_key: &str,
    name: &str,
    path: &Path,
    relative: &Path,
    shared: &BTreeMap<PathBuf, Arc<[u8]>>,
    budget: &mut PreparedBudget,
) -> PackageResult<PreparedDirectoryEntry> {
    budget.account_entry(path)?;
    let link_metadata = fs::symlink_metadata(path)?;
    let mut handle = open_staged_path_no_follow_with_kind(path, link_metadata.is_dir())?;
    let metadata = handle.metadata()?;
    if is_reparse_point(&metadata) {
        return Err(format!(
            "directory entry is a symlink or reparse point: {}",
            path.display()
        )
        .into());
    }
    if metadata.is_file() {
        let file_state = file_snapshot_state(&handle)?;
        budget.reserve_bytes(path, file_state.len)?;
        let identity = file_state.identity;
        let snapshot = read_stable_snapshot_with_limit(
            "directory commit",
            path,
            &mut handle,
            &NoopSnapshotObserver,
            Some(file_state.len),
        )?;
        #[cfg(unix)]
        verify_snapshot_against_second_read("directory commit", path, &mut handle, &snapshot)?;
        let bytes = shared
            .get(relative)
            .filter(|candidate| candidate.as_ref() == snapshot.as_ref())
            .map_or(snapshot, Arc::clone);
        Ok(PreparedDirectoryEntry::File {
            name: name.to_owned(),
            identity,
            bytes,
        })
    } else if metadata.is_dir() {
        let contents = prepare_directory_contents(path, relative, shared, budget)?;
        Ok(PreparedDirectoryEntry::Directory {
            name: name.to_owned(),
            contents,
        })
    } else {
        Err(format!("directory entry {name_key:?} is not a regular file or directory").into())
    }
}

pub(crate) fn prepare_directory_contents(
    path: &Path,
    relative_prefix: &Path,
    shared: &BTreeMap<PathBuf, Arc<[u8]>>,
    budget: &mut PreparedBudget,
) -> PackageResult<PreparedDirectoryContents> {
    let (identity, actual_names) = inspect_directory_identity(path, budget.remaining_entries())?;
    let expected_names = actual_names.keys().cloned().collect::<BTreeSet<_>>();
    if actual_names.len() > budget.remaining_entries() {
        return Err(format!(
            "prepared directory contains more than {MAX_PREPARED_ENTRIES} entries"
        )
        .into());
    }
    let mut entries = Vec::with_capacity(actual_names.len());
    for (name_key, name) in &actual_names {
        let entry_relative = relative_prefix.join(name);
        entries.push(prepare_directory_entry(
            name_key,
            name,
            &path.join(name),
            &entry_relative,
            shared,
            budget,
        )?);
    }
    ensure_directory_identity(path, identity, "directory commit")?;
    Ok(PreparedDirectoryContents {
        identity,
        entries,
        expected_names,
    })
}

pub(crate) fn verify_prepared_directory(
    prepared: &PreparedDirectoryCommit,
    directory: &Path,
    source: bool,
) -> PackageResult {
    verify_prepared_directory_contents(
        directory,
        prepared.staging_directory_identity,
        &prepared.expected_names,
        &prepared.entries,
        source,
    )
}

pub(crate) fn verify_prepared_directory_contents(
    directory: &Path,
    expected_identity: DirectoryIdentity,
    expected_names: &BTreeSet<String>,
    entries: &[PreparedDirectoryEntry],
    source: bool,
) -> PackageResult {
    let (identity, actual_names) = inspect_directory_identity(directory, MAX_PREPARED_ENTRIES)?;
    if identity != expected_identity {
        return Err(format!(
            "directory identity changed during commit verification: {}",
            directory.display()
        )
        .into());
    }
    ensure_exact_entry_set(&actual_names, expected_names, directory)?;
    for entry in entries {
        match entry {
            PreparedDirectoryEntry::File {
                name,
                identity: expected_identity,
                bytes: expected_bytes,
            } => {
                let path = directory.join(name);
                let mut handle = open_staged_file_no_follow(&path)
                    .map_err(|_| staged_changed("directory commit", &path))?;
                let metadata = handle
                    .metadata()
                    .map_err(|_| staged_changed("directory commit", &path))?;
                if !metadata.is_file() || is_reparse_point(&metadata) {
                    return Err(staged_changed("directory commit", &path));
                }
                let state = file_snapshot_state(&handle)
                    .map_err(|_| staged_changed("directory commit", &path))?;
                let bytes = read_stable_snapshot_with_limit(
                    "directory commit",
                    &path,
                    &mut handle,
                    &NoopSnapshotObserver,
                    Some(expected_bytes.len() as u64),
                )?;
                #[cfg(unix)]
                verify_snapshot_against_second_read(
                    "directory commit",
                    &path,
                    &mut handle,
                    &bytes,
                )?;
                if source {
                    if state.identity != *expected_identity {
                        return Err(staged_changed("directory commit", &path));
                    }
                } else if state.identity != *expected_identity
                    && bytes.as_ref() != expected_bytes.as_ref()
                {
                    return Err(staged_changed("directory commit", &path));
                }
                if bytes.as_ref() != expected_bytes.as_ref() {
                    return Err(staged_changed("directory commit", &path));
                }
            }
            PreparedDirectoryEntry::Directory { name, contents } => {
                let path = directory.join(name);
                verify_prepared_directory_contents(
                    &path,
                    contents.identity,
                    &contents.expected_names,
                    &contents.entries,
                    source,
                )?;
            }
        }
    }
    ensure_directory_identity(directory, identity, "directory commit")
}

pub(crate) fn lock_prepared_directory_source(
    lease: &mut CommitSourceLease,
    directory: &Path,
    expected_identity: DirectoryIdentity,
    entries: &[PreparedDirectoryEntry],
) -> PackageResult {
    ensure_directory_identity(directory, expected_identity, "directory commit")?;
    for entry in entries {
        match entry {
            PreparedDirectoryEntry::File { name, identity, .. } => {
                lease.lock_file(&directory.join(name), *identity)?
            }
            PreparedDirectoryEntry::Directory { name, contents } => {
                lock_prepared_directory_source(
                    lease,
                    &directory.join(name),
                    contents.identity,
                    &contents.entries,
                )?;
            }
        }
    }
    ensure_directory_identity(directory, expected_identity, "directory commit")
}

pub(crate) fn verify_prepared_package_directory(
    prepared: &PreparedPackageCommit,
    directory: &Path,
    source: bool,
) -> PackageResult {
    let (identity, actual_names) = inspect_directory_identity(directory, MAX_PREPARED_ENTRIES)?;
    if identity != prepared.staging_directory_identity {
        return Err(staged_changed(&prepared.target, directory));
    }
    ensure_exact_entry_set(&actual_names, &prepared.expected_names, directory)?;
    for entry in &prepared.entries {
        let expected_identity = if source {
            ExpectedIdentity::Exact(entry.identity)
        } else {
            ExpectedIdentity::SameOrBytes(entry.identity)
        };
        verify_staged_artifact(
            &prepared.target,
            &directory.join(&entry.artifact.relative_path),
            &entry.artifact,
            expected_identity,
        )?;
    }
    ensure_directory_identity(directory, identity, &prepared.target)?;
    validate_manifest_bytes(
        &prepared
            .entries
            .iter()
            .map(|entry| entry.artifact.clone())
            .collect::<Vec<_>>(),
    )
    .map_err(|error| PackageError::InvalidBuildManifest(error.to_string()))
}
