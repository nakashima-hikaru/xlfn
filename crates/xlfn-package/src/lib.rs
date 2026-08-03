//! Shared validation and staging for public and repository XLL packaging.
//!
//! The package pipeline is deliberately closed-world: bundle sources are
//! snapshotted, PE imports and exports are validated, staged artifacts are
//! hashed, and commit-time identity checks protect the final rename. The
//! Windows-specific private-directory checks enforce the same trust boundary
//! used by the transactional distributor.

#![deny(unsafe_code)]

use fs_err as fs;
use object::FileKind;
use object::endian::LittleEndian as LE;
use object::pe::{
    IMAGE_FILE_DLL, IMAGE_FILE_EXECUTABLE_IMAGE, IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_I386,
    IMAGE_FILE_SYSTEM, IMAGE_SCN_MEM_EXECUTE,
};
use object::read::pe::{
    ExportAddressIndex, ExportOrdinal, ExportTarget, ImageNtHeaders, Import as PeImport, PeFile,
    PeFile32, PeFile64,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[cfg(target_os = "windows")]
#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
mod win32;

#[cfg(target_os = "windows")]
fn wide_nul(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

pub type PackageResult<T = ()> = Result<T, PackageError>;
pub const SYSTEM_IMPORT_POLICY_VERSION: &str = "windows-system-v1";
pub const REQUIRED_XLL_EXPORTS: &[&str] = &[
    "xlAutoOpen",
    "xlAutoClose",
    "xlAutoFree12",
    "xlAddInManagerInfo12",
    "DllGetClassObject",
    "DllCanUnloadNow",
];

const MAX_PREPARED_ENTRIES: usize = 100_000;
const MAX_PREPARED_BYTES: u64 = 512 * 1024 * 1024;

const CRT_MARKER_MAGIC: &[u8; 8] = b"XLFNCRT\0";
const CRT_MARKER_SCHEMA: u8 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectiveCrtPolicy {
    Dynamic,
    Static,
}

impl EffectiveCrtPolicy {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dynamic => "dynamic",
            Self::Static => "static",
        }
    }
}

fn parse_crt_marker(data: &[u8]) -> PackageResult<EffectiveCrtPolicy> {
    if data.len() < 16 || &data[..CRT_MARKER_MAGIC.len()] != CRT_MARKER_MAGIC {
        return Err("malformed .xlfncrt marker: marker must start at section offset 0".into());
    }
    if data
        .windows(CRT_MARKER_MAGIC.len())
        .filter(|candidate| *candidate == CRT_MARKER_MAGIC)
        .count()
        != 1
    {
        return Err("malformed .xlfncrt marker: multiple magic values".into());
    }
    if data[16..].iter().any(|byte| *byte != 0) {
        return Err("malformed .xlfncrt marker: non-zero section padding".into());
    }
    let marker = &data[..16];
    if marker[8] != CRT_MARKER_SCHEMA {
        return Err(format!("unsupported .xlfncrt marker schema {}", marker[8]).into());
    }
    if marker[10..].iter().any(|byte| *byte != 0) {
        return Err("malformed .xlfncrt marker: reserved bytes are not zero".into());
    }
    match marker[9] {
        0 => Ok(EffectiveCrtPolicy::Dynamic),
        1 => Ok(EffectiveCrtPolicy::Static),
        value => Err(format!("invalid .xlfncrt policy value {value}").into()),
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ImportTarget {
    Name(String),
    Ordinal(u16),
}

impl ImportTarget {
    fn display(&self) -> String {
        match self {
            Self::Name(name) => name.clone(),
            Self::Ordinal(ordinal) => format!("#{ordinal}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Pe(#[from] object::Error),
    #[error(transparent)]
    Utf8(#[from] std::str::Utf8Error),
    #[error("{target}: bundle source is busy or open for modification: {path}: {source}")]
    BundleSourceBusy {
        target: String,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{target}: bundle source changed while snapshotting: {path}")]
    UnstableBundleSource { target: String, path: PathBuf },
    #[error("{target}: staged artifact changed after verification: {path}")]
    StagedArtifactChanged { target: String, path: PathBuf },
    #[error("private staging directory path was replaced: {path}")]
    StagingDirectoryReplaced { path: PathBuf },
    #[error("invalid build manifest: {0}")]
    InvalidBuildManifest(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Message(String),
}

impl From<String> for PackageError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for PackageError {
    fn from(message: &str) -> Self {
        Self::Message(message.to_owned())
    }
}

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

fn default_strict_paths() -> bool {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Architecture {
    X86,
    X64,
}

impl Architecture {
    pub fn parse(target: &str) -> PackageResult<Self> {
        match target {
            "i686-pc-windows-msvc" => Ok(Self::X86),
            "x86_64-pc-windows-msvc" => Ok(Self::X64),
            _ => Err(format!("unsupported Windows target {target:?}").into()),
        }
    }
    pub const fn machine(self) -> u16 {
        match self {
            Self::X86 => 0x014c,
            Self::X64 => 0x8664,
        }
    }

    fn pe_machine(self) -> object::pe::Machine {
        match self {
            Self::X86 => IMAGE_FILE_MACHINE_I386,
            Self::X64 => IMAGE_FILE_MACHINE_AMD64,
        }
    }
}

#[derive(Clone)]
struct BundleFile {
    source: PathBuf,
    name: String,
    configured_path: String,
    snapshot: Option<Arc<[u8]>>,
    permissions: std::fs::Permissions,
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
    files: Vec<BundleFile>,
    external_imports: BTreeSet<String>,
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
    files: Vec<BundleFile>,
    external_imports: BTreeSet<String>,
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

#[derive(Clone, Debug)]
pub struct VerifiedArtifact {
    relative_path: PathBuf,
    bytes: Arc<[u8]>,
    size: u64,
    sha256: [u8; 32],
    permissions: std::fs::Permissions,
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
    artifacts: Vec<VerifiedArtifact>,
    expected_names: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectoryIdentity(FileIdentity);

/// A directory that was created or adopted after its private staging
/// invariants were established.  Packaging APIs accept this capability
/// instead of an arbitrary path so callers cannot accidentally stage into a
/// directory that was public during construction.
#[derive(Debug)]
pub struct PrivateStagingDirectory {
    path: PathBuf,
    identity: DirectoryIdentity,
    handle: std::fs::File,
}

impl PrivateStagingDirectory {
    /// Creates a new private directory with its private attributes applied as
    /// part of the create operation itself.
    pub fn create(path: &Path) -> PackageResult<Self> {
        if fs::symlink_metadata(path).is_ok() {
            return Err(format!(
                "private staging directory already exists: {}",
                path.display()
            )
            .into());
        }

        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        validate_directory_path(parent)?;
        fs::create_dir_all(parent)?;
        validate_directory_path(parent)?;
        create_private_directory(path)?;
        Self::open(path)
    }

    /// Adopts an already-created directory after verifying its identity and
    /// private attributes.  This is intended for directories created by an
    /// OS-backed temporary-directory primitive.
    pub fn open(path: &Path) -> PackageResult<Self> {
        validate_path_components(path)?;
        let handle = open_private_directory(path).map_err(|error| {
            PackageError::Message(format!(
                "failed to open private staging directory {}: {error}",
                path.display()
            ))
        })?;
        let metadata = handle.metadata()?;
        validate_private_directory(path, &metadata)?;
        let identity = DirectoryIdentity(file_snapshot_state(&handle)?.identity);
        Ok(Self {
            path: path.to_path_buf(),
            identity,
            handle,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn verify(&self) -> PackageResult {
        if validate_path_components(&self.path).is_err() {
            return Err(PackageError::StagingDirectoryReplaced {
                path: self.path.clone(),
            });
        }
        let path_handle = open_private_directory(&self.path).map_err(|_| {
            PackageError::StagingDirectoryReplaced {
                path: self.path.clone(),
            }
        })?;
        let path_metadata =
            path_handle
                .metadata()
                .map_err(|_| PackageError::StagingDirectoryReplaced {
                    path: self.path.clone(),
                })?;
        if validate_private_directory(&self.path, &path_metadata).is_err()
            || DirectoryIdentity(file_snapshot_state(&path_handle)?.identity) != self.identity
        {
            return Err(PackageError::StagingDirectoryReplaced {
                path: self.path.clone(),
            });
        }

        let handle_metadata = self.handle.metadata()?;
        validate_private_directory(&self.path, &handle_metadata)?;
        let handle_identity = DirectoryIdentity(file_snapshot_state(&self.handle)?.identity);
        if handle_identity != self.identity {
            return Err(PackageError::StagingDirectoryReplaced {
                path: self.path.clone(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct PreparedArtifact {
    artifact: VerifiedArtifact,
    identity: FileIdentity,
}

#[derive(Clone, Debug)]
pub struct PreparedPackageCommit {
    staging_directory: PathBuf,
    staging_directory_identity: DirectoryIdentity,
    entries: Vec<PreparedArtifact>,
    expected_names: BTreeSet<String>,
    target: String,
}

#[derive(Clone, Debug)]
enum PreparedDirectoryEntry {
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
struct PreparedDirectoryContents {
    identity: DirectoryIdentity,
    entries: Vec<PreparedDirectoryEntry>,
    expected_names: BTreeSet<String>,
}

/// A closed-world snapshot of a directory tree. Every descendant directory
/// and regular file is retained in the snapshot and revalidated at commit
/// time; directory identity alone is never used as proof of unchanged
/// contents.
#[derive(Clone, Debug)]
pub struct PreparedDirectoryCommit {
    staging_directory: PathBuf,
    staging_directory_identity: DirectoryIdentity,
    entries: Vec<PreparedDirectoryEntry>,
    expected_names: BTreeSet<String>,
}

/// Holds read handles for every source file in a prepared tree until its
/// directory rename completes. On Windows the handles omit write sharing, so
/// a new writer cannot modify an already-verified source between the final
/// verification and publication.
#[must_use = "keep the source lease alive through the directory rename"]
#[derive(Debug, Default)]
pub struct CommitSourceLease {
    handles: Vec<std::fs::File>,
}

#[derive(Default)]
struct PreparedBudget {
    entries: usize,
    bytes: u64,
}

impl PreparedBudget {
    fn account_entry(&mut self, path: &Path) -> PackageResult {
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

    fn reserve_bytes(&mut self, path: &Path, size: u64) -> PackageResult {
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

    fn remaining_entries(&self) -> usize {
        MAX_PREPARED_ENTRIES.saturating_sub(self.entries)
    }
}

impl CommitSourceLease {
    fn lock_package(
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

    fn lock_directory(
        mut self,
        root: &Path,
        expected_root: DirectoryIdentity,
        entries: &[PreparedDirectoryEntry],
    ) -> PackageResult<Self> {
        lock_prepared_directory_source(&mut self, root, expected_root, entries)?;
        Ok(self)
    }

    fn lock_file(&mut self, path: &Path, expected_identity: FileIdentity) -> PackageResult {
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

    /// Acquires the source lease used for the final commit window. Callers
    /// must keep the returned lease alive through the source directory rename.
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

    /// Acquires the source lease used for the final commit window. Callers
    /// must keep the returned lease alive through the source directory rename.
    pub fn lock_source_for_commit(&self) -> PackageResult<CommitSourceLease> {
        CommitSourceLease::default().lock_directory(
            &self.staging_directory,
            self.staging_directory_identity,
            &self.entries,
        )
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

fn resolve_bundle_files_with_policy_impl(
    manifest_directory: &Path,
    target: &str,
    configured: &[String],
    external_imports: &[String],
    strict_paths: bool,
    observer: &dyn SnapshotObserver,
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

pub fn verify_xll(path: &Path, target: &str, required_exports: &[String]) -> PackageResult {
    verify_xll_bytes(&fs::read(path)?, target, required_exports, path)
}

fn verify_xll_bytes(
    bytes: &[u8],
    target: &str,
    required_exports: &[String],
    path: &Path,
) -> PackageResult {
    let info = parse_pe_bytes(bytes)?;
    verify_machine(&info, Architecture::parse(target)?, path)?;
    verify_image_characteristics(&info, path)?;
    verify_xll_exports(&info, path, required_exports)
}

fn verify_xll_exports(info: &PeInfo, path: &Path, required_exports: &[String]) -> PackageResult {
    if !info.has_export_manifest {
        return Err(format!(
            "{} is missing the .xllexp export manifest; ensure the crate has exactly one #[excel_addin]",
            path.display()
        )
        .into());
    }
    if info.crt_policy.is_none() {
        return Err(format!(
            "{} is missing the .xlfncrt effective CRT policy marker",
            path.display()
        )
        .into());
    }

    for export in REQUIRED_XLL_EXPORTS {
        if !info.exports.contains(*export) {
            return Err(format!("{} is missing export {export}", path.display()).into());
        }
        reject_forwarded_xll_export(info, path, export)?;
        if !info.executable_exports.contains(*export) {
            return Err(format!(
                "{} export {export} is not a direct executable target",
                path.display()
            )
            .into());
        }
        if !info.expected_exports.contains(*export) {
            return Err(format!(
                "{} has an incomplete .xllexp export manifest: missing {export}",
                path.display()
            )
            .into());
        }
    }
    for export in required_exports {
        if !info.exports.contains(export) {
            return Err(format!("{} is missing export {export}", path.display()).into());
        }
        reject_forwarded_xll_export(info, path, export)?;
        if !info.executable_exports.contains(export) {
            return Err(format!(
                "{} export {export} is not a direct executable target",
                path.display()
            )
            .into());
        }
    }

    let missing = info
        .expected_exports
        .difference(&info.exports)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "{} is missing expected export(s): {}",
            path.display(),
            missing.join(", ")
        )
        .into());
    }
    for export in &info.expected_exports {
        reject_forwarded_xll_export(info, path, export)?;
    }

    let non_executable = info
        .expected_exports
        .difference(&info.executable_exports)
        .cloned()
        .collect::<Vec<_>>();
    if !non_executable.is_empty() {
        return Err(format!(
            "{} has expected export(s) without a direct executable target: {}",
            path.display(),
            non_executable.join(", ")
        )
        .into());
    }

    // Closed-world validation: Reject any PE named export not present in expected_exports
    // (except loader entry points like _DllMainCRTStartup if present).
    let unexpected = info
        .exports
        .iter()
        .filter(|name| {
            !info.expected_exports.contains(*name) && name.as_str() != "_DllMainCRTStartup"
        })
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(format!(
            "{} has unexpected export(s): {}",
            path.display(),
            unexpected.join(", ")
        )
        .into());
    }

    let ordinal_only = info
        .nonzero_export_slots
        .difference(&info.named_export_slots)
        .collect::<Vec<_>>();
    if !ordinal_only.is_empty() {
        return Err(format!("{} has unmanifested ordinal-only export(s)", path.display()).into());
    }

    Ok(())
}

/// Validates the import closure rooted at the generated XLL and every bundled
/// DLL. Non-system imports must resolve to a case-insensitive basename in the
/// bundle, every imported name or ordinal must exist in that image, and every
/// PE image must match the requested architecture.
fn verify_dependency_closure(
    xll: &Path,
    target: &str,
    bundle: &StagedBundle,
    xll_snapshot: &[u8],
) -> PackageResult {
    let architecture = Architecture::parse(target)?;
    let root_name = xll
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("XLL has no UTF-8 basename: {}", xll.display()))?;
    let mut images = BTreeMap::new();
    let root_key = windows_name_key("XLL basename", root_name)?;
    images.insert(
        root_key.clone(),
        (
            root_name.to_owned(),
            inspect_checked_pe_bytes(xll_snapshot, architecture, xll)?,
        ),
    );
    for file in bundle
        .files
        .iter()
        .filter(|file| file.name.to_ascii_lowercase().ends_with(".dll"))
    {
        let key = windows_dll_name_key("bundled DLL", &file.name)?;
        if key == root_key {
            return Err(format!(
                "bundled DLL basename `{}` collides with root XLL basename `{root_name}`",
                file.name
            )
            .into());
        }
        if images
            .insert(
                key,
                (
                    file.name.clone(),
                    inspect_checked_bundle_file(file, architecture)?,
                ),
            )
            .is_some()
        {
            return Err(format!("duplicate DLL basename in bundle: `{}`", file.name).into());
        }
    }
    validate_dependency_graph(&images, &bundle.external_imports)
}

/// Validates a standalone PE root and its colocated bundled DLL closure.
///
/// Every image must match `target`; non-system imports must resolve to one of
/// `bundled`, and every imported name or ordinal must exist in that image.
pub fn verify_pe_dependency_closure(
    root: &Path,
    target: &str,
    bundled: &[PathBuf],
) -> PackageResult {
    let architecture = Architecture::parse(target)?;
    let root_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("PE root has no UTF-8 basename: {}", root.display()))?;
    let root_key = windows_name_key("PE root basename", root_name)?;
    let mut images = BTreeMap::new();
    images.insert(
        root_key.clone(),
        (
            root_name.to_owned(),
            inspect_checked_pe(root, architecture)?,
        ),
    );

    for path in bundled {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("bundled DLL has no UTF-8 basename: {}", path.display()))?;
        let key = windows_dll_name_key("bundled DLL", name)?;
        if is_system(&key) {
            return Err(format!("bundle must not shadow Windows system DLL {name:?}").into());
        }
        if key == root_key {
            return Err(
                format!("bundled DLL basename `{name}` collides with root `{root_name}`").into(),
            );
        }
        if images
            .insert(
                key,
                (name.to_owned(), inspect_checked_pe(path, architecture)?),
            )
            .is_some()
        {
            return Err(format!("duplicate DLL basename in bundle: `{name}`").into());
        }
    }

    validate_dependency_graph(&images, &BTreeSet::new())
}

pub fn sha256(path: &Path) -> PackageResult<String> {
    let mut hash = Sha256::new();
    let mut file = fs::File::open(path)?;
    struct DigestWriter<'a>(&'a mut Sha256);
    impl io::Write for DigestWriter<'_> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.update(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    io::copy(&mut file, &mut DigestWriter(&mut hash))?;
    let digest = hash.finalize();
    Ok(digest_hex(&digest))
}

fn sha256_digest(bytes: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(bytes);
    let digest = hash.finalize();
    let mut output = [0_u8; 32];
    output.copy_from_slice(&digest);
    output
}

#[derive(Clone, Copy, Debug)]
enum ExpectedIdentity {
    Any,
    Exact(FileIdentity),
    SameOrBytes(FileIdentity),
}

fn snapshot_staged_artifact(target: &str, path: &Path) -> PackageResult<Arc<[u8]>> {
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

fn staged_artifact_permissions(target: &str, path: &Path) -> PackageResult<std::fs::Permissions> {
    let file = open_staged_file_no_follow(path).map_err(|_| staged_changed(target, path))?;
    let metadata = file.metadata().map_err(|_| staged_changed(target, path))?;
    if !metadata.is_file() || is_reparse_point(&metadata) {
        return Err(staged_changed(target, path));
    }
    Ok(metadata.permissions())
}

fn staged_changed(target: &str, path: &Path) -> PackageError {
    PackageError::StagedArtifactChanged {
        target: target.to_owned(),
        path: path.to_owned(),
    }
}

fn verify_staged_artifact(
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

fn expected_name_set(names: &[&str]) -> PackageResult<BTreeSet<String>> {
    names
        .iter()
        .map(|name| windows_name_key("expected directory entry", name))
        .collect()
}

fn inspect_directory_identity(
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
    let identity = DirectoryIdentity(file_snapshot_state(&directory)?.identity);
    drop(directory);
    Ok((identity, read_directory_entries(path, max_entries)?))
}

fn ensure_directory_identity(
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
    let identity = DirectoryIdentity(file_snapshot_state(&directory)?.identity);
    if identity != expected {
        return Err(format!("{label}: directory identity changed: {}", path.display()).into());
    }
    Ok(())
}

fn read_directory_entries(
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

fn ensure_exact_entry_set(
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

fn prepare_directory_entry(
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

fn prepare_directory_contents(
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

fn verify_prepared_directory(
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

fn verify_prepared_directory_contents(
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

fn lock_prepared_directory_source(
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

fn verify_prepared_package_directory(
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

fn validate_manifest_bytes(artifacts: &[VerifiedArtifact]) -> PackageResult {
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

fn digest_hex(digest: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(digest.len() * 2);
    for &byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn verified_artifact(
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

fn artifact_relative_path(path: &Path, label: &str) -> PackageResult<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{label} has no UTF-8 basename: {}", path.display()))?;
    windows_name_key(label, name)?;
    Ok(PathBuf::from(name))
}

fn validate_imports(
    files: &[BundleFile],
    architecture: Architecture,
    external_imports: &BTreeSet<String>,
) -> PackageResult {
    let mut images = BTreeMap::new();
    for file in files
        .iter()
        .filter(|file| file.name.to_ascii_lowercase().ends_with(".dll"))
    {
        let key = windows_dll_name_key("bundled DLL", &file.name)?;
        images.insert(
            key,
            (
                file.name.clone(),
                inspect_checked_bundle_file(file, architecture)?,
            ),
        );
    }
    validate_dependency_graph(&images, external_imports)
}

fn inspect_checked_pe(path: &Path, architecture: Architecture) -> PackageResult<PeInfo> {
    let info = inspect_pe(path)?;
    inspect_checked_info(info, architecture, path)
}

fn inspect_checked_pe_bytes(
    bytes: &[u8],
    architecture: Architecture,
    path: &Path,
) -> PackageResult<PeInfo> {
    let info = parse_pe_bytes(bytes)?;
    inspect_checked_info(info, architecture, path)
}

fn inspect_checked_info(
    info: PeInfo,
    architecture: Architecture,
    path: &Path,
) -> PackageResult<PeInfo> {
    verify_machine(&info, architecture, path)?;
    verify_image_characteristics(&info, path)?;
    Ok(info)
}

fn inspect_checked_bundle_file(
    file: &BundleFile,
    architecture: Architecture,
) -> PackageResult<PeInfo> {
    let info = match file.snapshot.as_deref() {
        Some(snapshot) => parse_pe_bytes(snapshot)?,
        None => inspect_pe(&file.source)?,
    };
    verify_machine(&info, architecture, &file.source)?;
    verify_image_characteristics(&info, &file.source)?;
    Ok(info)
}

fn validate_dependency_graph(
    images: &BTreeMap<String, (String, PeInfo)>,
    external_imports: &BTreeSet<String>,
) -> PackageResult {
    let bundled_names = images.keys().cloned().collect::<BTreeSet<_>>();
    if let Some(name) = external_imports.intersection(&bundled_names).next() {
        return Err(format!("DLL `{name}` cannot be both bundled and external").into());
    }
    for root in images.keys() {
        let mut path = vec![root.clone()];
        validate_dependency_node(
            root,
            images,
            external_imports,
            &mut path,
            &mut BTreeSet::new(),
        )?;
    }
    validate_forwarded_exports(images, external_imports)
}

fn validate_dependency_node(
    current: &str,
    images: &BTreeMap<String, (String, PeInfo)>,
    external_imports: &BTreeSet<String>,
    path: &mut Vec<String>,
    visited: &mut BTreeSet<String>,
) -> PackageResult {
    if !visited.insert(current.to_owned()) {
        return Ok(());
    }
    let (_, image) = images
        .get(current)
        .ok_or_else(|| format!("internal dependency graph error for {current}"))?;
    let imports = image
        .imports
        .iter()
        .map(|name| (name, image.import_targets.get(name)))
        .chain(
            image
                .delay_imports
                .iter()
                .map(|name| (name, image.delay_import_targets.get(name))),
        );
    for (imported_name, targets) in imports {
        let imported = windows_dll_name_key("PE import", imported_name)?;
        if is_system(&imported) || external_imports.contains(&imported) {
            continue;
        }
        let Some((imported_display, imported_image)) = images.get(&imported) else {
            let mut chain = path
                .iter()
                .filter_map(|name| images.get(name).map(|(display, _)| display.as_str()))
                .collect::<Vec<_>>();
            chain.push(imported.as_str());
            return Err(format!(
                "unresolved package import (policy {SYSTEM_IMPORT_POLICY_VERSION}): {}",
                chain.join(" -> ")
            )
            .into());
        };

        if let Some(targets) = targets {
            for target in targets {
                let exists = match target {
                    ImportTarget::Name(name) => imported_image.exports.contains(name),
                    ImportTarget::Ordinal(ordinal) => imported_image
                        .exported_ordinals
                        .contains(&ExportOrdinal(*ordinal)),
                };
                if !exists {
                    let mut chain = path
                        .iter()
                        .filter_map(|name| images.get(name).map(|(display, _)| display.clone()))
                        .collect::<Vec<_>>();
                    chain.push(format!("{imported_display}!{}", target.display()));
                    return Err(format!(
                        "unresolved package import target (policy {SYSTEM_IMPORT_POLICY_VERSION}): {}",
                        chain.join(" -> ")
                    )
                    .into());
                }
            }
        }

        path.push(imported.clone());
        validate_dependency_node(&imported, images, external_imports, path, visited)?;
        path.pop();
    }
    Ok(())
}

fn validate_forwarded_exports(
    images: &BTreeMap<String, (String, PeInfo)>,
    external_imports: &BTreeSet<String>,
) -> PackageResult {
    let mut resolved = BTreeSet::new();
    for (image_name, (_, image)) in images {
        for symbol in image.forwarded_exports.keys() {
            let mut stack = Vec::new();
            validate_forwarded_symbol(
                image_name,
                symbol,
                images,
                external_imports,
                &mut stack,
                &mut resolved,
            )?;
        }
    }
    Ok(())
}

fn validate_forwarded_symbol(
    image_name: &str,
    symbol: &ExportSymbol,
    images: &BTreeMap<String, (String, PeInfo)>,
    external_imports: &BTreeSet<String>,
    stack: &mut Vec<(String, ExportSymbol)>,
    resolved: &mut BTreeSet<(String, ExportSymbol)>,
) -> PackageResult {
    let node = (image_name.to_owned(), symbol.clone());
    if resolved.contains(&node) {
        return Ok(());
    }
    if let Some(position) = stack.iter().position(|entry| entry == &node) {
        let mut cycle = stack[position..]
            .iter()
            .map(|(image, symbol)| format!("{image}!{}", format_export_symbol(symbol)))
            .collect::<Vec<_>>();
        cycle.push(format!("{image_name}!{}", format_export_symbol(symbol)));
        return Err(format!("cyclic forwarded export: {}", cycle.join(" -> ")).into());
    }

    let (_, image) = images
        .get(image_name)
        .ok_or_else(|| format!("internal forwarded-export graph error for {image_name}"))?;
    let Some(forwarded) = image.forwarded_exports.get(symbol) else {
        if image.has_export_symbol(symbol) {
            resolved.insert(node);
            return Ok(());
        }
        return Err(format!(
            "{} is missing forwarded export target {}",
            images
                .get(image_name)
                .map_or(image_name, |(display, _)| display.as_str()),
            format_export_symbol(symbol)
        )
        .into());
    };

    stack.push(node.clone());
    let target_library = forwarded.library.to_ascii_lowercase();
    if !is_system(&target_library) && !external_imports.contains(&target_library) {
        let Some((target_display, target_image)) = images.get(&target_library) else {
            return Err(format!(
                "unresolved forwarded export: {}!{} -> {}!{}",
                images
                    .get(image_name)
                    .map_or(image_name, |(display, _)| display.as_str()),
                format_export_symbol(symbol),
                forwarded.library,
                format_export_symbol(&forwarded.symbol)
            )
            .into());
        };
        if !target_image.has_export_symbol(&forwarded.symbol) {
            return Err(format!(
                "forwarded export target is missing: {}!{} -> {target_display}!{}",
                images
                    .get(image_name)
                    .map_or(image_name, |(display, _)| display.as_str()),
                format_export_symbol(symbol),
                format_export_symbol(&forwarded.symbol)
            )
            .into());
        }
        validate_forwarded_symbol(
            &target_library,
            &forwarded.symbol,
            images,
            external_imports,
            stack,
            resolved,
        )?;
    }
    let _ = stack.pop();
    resolved.insert(node);
    Ok(())
}

fn format_export_symbol(symbol: &ExportSymbol) -> String {
    match symbol {
        ExportSymbol::Name(name) => name.clone(),
        ExportSymbol::Ordinal(ordinal) => format!("#{ordinal}"),
    }
}

fn validate_relative(field: &str, value: &str) -> PackageResult {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || value.contains(':')
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        Err(format!("{field} has unsafe path {value:?}").into())
    } else {
        Ok(())
    }
}

/// Rejects symlinks and Windows reparse points in every existing component of
/// a path. Missing trailing components are allowed so callers can validate a
/// destination before creating it. This is a path-based check: it rejects
/// links present during validation, but does not provide descriptor-relative
/// protection against a concurrent adversary replacing a checked component.
pub fn validate_path_components(path: &Path) -> PackageResult {
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if is_reparse_point(&metadata) && !is_trusted_system_alias(ancestor) => {
                return Err(format!(
                    "path component must not be a symlink or reparse point: {}",
                    ancestor.display()
                )
                .into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Validates a directory destination and all of its existing ancestors.
pub fn validate_directory_path(path: &Path) -> PackageResult {
    validate_path_components(path)?;
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if !metadata.is_dir() && !is_trusted_system_alias(ancestor) => {
                return Err(
                    format!("path component must be a directory: {}", ancestor.display()).into(),
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn is_trusted_system_alias(path: &Path) -> bool {
    let expected = match path {
        path if path == Path::new("/etc") => Path::new("/private/etc"),
        path if path == Path::new("/tmp") => Path::new("/private/tmp"),
        path if path == Path::new("/var") => Path::new("/private/var"),
        _ => return false,
    };
    fs::canonicalize(path).is_ok_and(|resolved| resolved == expected)
}

#[cfg(not(target_os = "macos"))]
const fn is_trusted_system_alias(_path: &Path) -> bool {
    false
}

fn windows_name_key(field: &str, name: &str) -> PackageResult<String> {
    validate_windows_basename_for(field, name)?;
    Ok(name.to_ascii_lowercase())
}

fn windows_dll_name_key(field: &str, name: &str) -> PackageResult<String> {
    let key = windows_name_key(field, name)?;
    if !name.to_ascii_lowercase().ends_with(".dll") {
        return Err(format!("{field} must be a DLL basename, got {name:?}").into());
    }
    Ok(key)
}

/// Validates a single output component against the portable ASCII subset of
/// Windows filename rules used by XLL packages.
pub fn validate_windows_basename(name: &str) -> PackageResult {
    validate_windows_basename_for("Windows basename", name)
}

fn validate_windows_basename_for(field: &str, name: &str) -> PackageResult {
    xlfn_common::validate_windows_basename(name).map_err(|error| {
        format!("{field} is not a valid portable Windows basename {name:?}: {error}").into()
    })
}

fn normalize_external_imports(imports: &[String]) -> PackageResult<BTreeSet<String>> {
    imports
        .iter()
        .map(|name| {
            validate_relative("external import", name)?;
            let path = Path::new(name);
            if path.file_name().and_then(|value| value.to_str()) != Some(name) {
                return Err(format!("external import must be a DLL basename, got {name:?}").into());
            }
            windows_dll_name_key("external import", name)
        })
        .collect()
}

fn path_is_within(root: &Path, candidate: &Path) -> bool {
    let mut candidate_components = candidate.components();
    root.components().all(|root_component| {
        candidate_components
            .next()
            .is_some_and(|candidate_component| component_eq(root_component, candidate_component))
    })
}

trait SnapshotObserver {
    fn after_open(&self, _path: &Path) {}

    fn after_first_chunk(&self, _path: &Path) {}
}

struct NoopSnapshotObserver;

impl SnapshotObserver for NoopSnapshotObserver {}

fn open_bundle_source_for_snapshot(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);

    #[cfg(target_os = "windows")]
    {
        use crate::win32::FILE_SHARE_READ;
        use std::os::windows::fs::OpenOptionsExt;

        // Other readers remain allowed, but writers and delete/rename operations
        // are rejected while this handle is alive.
        options.share_mode(FILE_SHARE_READ);
    }

    options.open(path)
}

fn open_staged_file_no_follow(path: &Path) -> io::Result<std::fs::File> {
    open_staged_path_no_follow_with_kind(path, false)
}

fn open_commit_source_file(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_NOFOLLOW);
    }

    #[cfg(target_os = "windows")]
    {
        use crate::win32::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ};
        use std::os::windows::fs::OpenOptionsExt;

        // Keep the source directory renameable while refusing new writers.
        // Writers that were already open are covered when they race the final
        // stable-content check; portable Rust cannot revoke those handles.
        options
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE);
    }

    options.open(path)
}

fn open_staged_directory_no_follow(path: &Path) -> io::Result<std::fs::File> {
    open_staged_path_no_follow_with_kind(path, true)
}

fn open_staged_path_no_follow_with_kind(path: &Path, directory: bool) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);

    #[cfg(not(target_os = "windows"))]
    let _ = directory;

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // O_NOFOLLOW makes the final component an object identity check rather
        // than a path lookup. The staging parent is private and the directory
        // identity is checked again after all entries have been opened.
        options.custom_flags(libc::O_NOFOLLOW);
    }

    #[cfg(target_os = "windows")]
    {
        use crate::win32::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        use std::os::windows::fs::OpenOptionsExt;

        let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
        if directory {
            flags |= FILE_FLAG_BACKUP_SEMANTICS;
        }
        let share_mode = if directory {
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
        } else {
            FILE_SHARE_READ
        };
        options
            .custom_flags(flags)
            // Keep writers and deletion from racing the read. These handles
            // are closed before the directory rename. The long-lived
            // PrivateStagingDirectory capability deliberately permits the
            // owning process to rename the directory after verification.
            .share_mode(share_mode);
    }

    options.open(path)
}

fn open_private_directory(path: &Path) -> io::Result<std::fs::File> {
    open_staged_directory_no_follow(path)
}

fn create_private_directory(path: &Path) -> PackageResult {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        std::fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    #[cfg(target_os = "windows")]
    {
        create_private_windows_directory(path)?;
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        fs::create_dir(path)?;
    }
    Ok(())
}

#[allow(unsafe_code)]
fn validate_private_directory(path: &Path, metadata: &std::fs::Metadata) -> PackageResult {
    if !metadata.is_dir() || is_reparse_point(metadata) {
        return Err(format!(
            "private staging path is not a regular directory: {}",
            path.display()
        )
        .into());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if metadata.mode() & 0o077 != 0 {
            return Err(format!(
                "private staging directory is accessible by other users: {}",
                path.display()
            )
            .into());
        }
        // SAFETY: geteuid has no preconditions and only reads the effective
        // user ID of the current process.
        let current_uid = unsafe { libc::geteuid() };
        if metadata.uid() != current_uid {
            return Err(format!(
                "private staging directory is not owned by the current user: {}",
                path.display()
            )
            .into());
        }
    }

    #[cfg(target_os = "windows")]
    validate_private_windows_directory(path)?;

    Ok(())
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
fn current_windows_user_sid_string() -> PackageResult<String> {
    use crate::win32::{
        CloseHandle, GetCurrentProcess, GetLengthSid, GetTokenInformation, IsValidSid,
        OpenProcessToken, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use std::fmt::Write as _;
    use std::mem::{MaybeUninit, align_of, size_of};

    let mut token = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns the current process pseudo-handle and
    // `token` points to writable storage for the returned token handle.
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if opened == 0 {
        return Err(io::Error::last_os_error().into());
    }

    let result = (|| -> PackageResult<String> {
        let mut required = 0_u32;
        // SAFETY: the null buffer intentionally performs the documented size
        // query; `required` is writable output storage.
        let _ = unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required)
        };
        if required == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let required_size = required as usize;
        if required_size < size_of::<TOKEN_USER>() {
            return Err("token information is shorter than TOKEN_USER".into());
        }
        let word_size = size_of::<usize>();
        let words = required_size
            .checked_add(word_size - 1)
            .ok_or_else(|| "token information size overflow".to_owned())?
            / word_size;
        if align_of::<usize>() < align_of::<TOKEN_USER>() {
            return Err("token information alignment is unsupported".into());
        }
        let mut token_buffer = vec![MaybeUninit::<usize>::uninit(); words];
        // SAFETY: the storage is aligned at least to `TOKEN_USER`, has at
        // least the requested byte length, and remains alive for the call.
        let queried = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                token_buffer.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        };
        if queried == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let returned_size = required as usize;
        if returned_size < size_of::<TOKEN_USER>()
            || returned_size > token_buffer.len().saturating_mul(word_size)
        {
            return Err("token information exceeds its storage".into());
        }
        // SAFETY: the successful query populated a complete TOKEN_USER at the
        // aligned beginning of the storage, and the storage remains alive.
        let token_user = unsafe {
            token_buffer
                .as_ptr()
                .cast::<TOKEN_USER>()
                .as_ref()
                .ok_or_else(|| "token information buffer is null".to_owned())?
        };
        if token_user.User.Sid.is_null()
            // SAFETY: the SID pointer came from the successful token query and
            // is non-null before it is validated here.
            || unsafe { IsValidSid(token_user.User.Sid) == 0 }
        {
            return Err("current user token has an invalid SID".into());
        }
        // SAFETY: the SID has been validated and remains backed by the live
        // token information buffer for the duration of this read.
        let sid_length = unsafe { GetLengthSid(token_user.User.Sid) } as usize;
        if sid_length < 8 {
            return Err("current user SID is shorter than the fixed SID header".into());
        }
        // SAFETY: GetLengthSid returned the complete size of the validated SID,
        // which remains live in `token_buffer`.
        let sid =
            unsafe { std::slice::from_raw_parts(token_user.User.Sid.cast::<u8>(), sid_length) };
        let subauthority_count = sid[1] as usize;
        let expected_length = 8_usize
            .checked_add(
                subauthority_count
                    .checked_mul(4)
                    .ok_or_else(|| "current user SID length overflow".to_owned())?,
            )
            .ok_or_else(|| "current user SID length overflow".to_owned())?;
        if sid_length != expected_length {
            return Err("current user SID has an inconsistent length".into());
        }
        let identifier_authority =
            u64::from_be_bytes([0, 0, sid[2], sid[3], sid[4], sid[5], sid[6], sid[7]]);
        let mut value = format!("S-{}-{identifier_authority}", sid[0]);
        for index in 0..subauthority_count {
            let offset = 8 + index * 4;
            let subauthority = u32::from_le_bytes([
                sid[offset],
                sid[offset + 1],
                sid[offset + 2],
                sid[offset + 3],
            ]);
            write!(&mut value, "-{subauthority}")
                .map_err(|error| format!("failed to format current user SID: {error}"))?;
        }
        Ok(value)
    })();

    // SAFETY: `token` was returned by OpenProcessToken and is closed exactly
    // once after all token-backed pointers are no longer used.
    unsafe {
        let _ = CloseHandle(token);
    }
    result
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
fn create_private_windows_directory(path: &Path) -> PackageResult {
    use crate::win32::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, CreateDirectoryW, HLOCAL, LocalFree,
        SDDL_REVISION_1, SECURITY_ATTRIBUTES,
    };
    use std::os::windows::ffi::OsStrExt;

    // Do not inherit permissions from the temporary directory. Explicitly set
    // the current user as owner and grant access only to that SID and SYSTEM;
    // relying on the token's default owner can select the Administrators group
    // on hosted runners and make the private-directory invariant fail.
    let user_sid = current_windows_user_sid_string()?;
    let descriptor_string = wide_nul(&format!("O:{user_sid}D:P(A;;FA;;;SY)(A;;FA;;;{user_sid})"));
    let mut descriptor = std::ptr::null_mut::<std::ffi::c_void>();
    // SAFETY: the SDDL literal is NUL-terminated and `descriptor` points to
    // writable storage for the API-owned security descriptor pointer.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_string.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if converted == 0 || descriptor.is_null() {
        return Err(io::Error::last_os_error().into());
    }

    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let security_attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // SAFETY: both pointers refer to live, NUL-terminated buffers for the
    // duration of the call. The descriptor is freed below regardless of the
    // result, after CreateDirectoryW has consumed it synchronously.
    let created = unsafe {
        CreateDirectoryW(
            path_wide.as_ptr(),
            &security_attributes as *const SECURITY_ATTRIBUTES,
        )
    };
    let error = if created == 0 {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    // SAFETY: the pointer was allocated by the conversion API and has not
    // been freed yet.
    unsafe {
        let _ = LocalFree(descriptor as HLOCAL);
    }
    error.map_or(Ok(()), Err).map_err(PackageError::from)
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
fn validate_private_windows_directory(path: &Path) -> PackageResult {
    // PrivateStagingDirectory::create supplies a protected DACL atomically.
    // Existing directories are accepted only through the explicit `open`
    // adoption path, which is used for OS-created temporary directories.
    use crate::win32::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, CloseHandle,
        ConvertStringSidToSidW, DACL_SECURITY_INFORMATION, ERROR_SUCCESS, EqualSid, GetAce,
        GetAclInformation, GetCurrentProcess, GetLengthSid, GetNamedSecurityInfoW,
        GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetTokenInformation, HLOCAL,
        IsValidSid, LocalFree, OWNER_SECURITY_INFORMATION, OpenProcessToken, SE_DACL_PROTECTED,
        SE_FILE_OBJECT, SECURITY_DESCRIPTOR_CONTROL, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use std::mem::{MaybeUninit, align_of, size_of};
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::read_unaligned;

    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut owner = std::ptr::null_mut::<std::ffi::c_void>();
    let mut dacl = std::ptr::null_mut::<ACL>();
    let mut security_descriptor = std::ptr::null_mut::<std::ffi::c_void>();
    let mut token = std::ptr::null_mut();
    let mut system_sid = std::ptr::null_mut::<std::ffi::c_void>();

    let result = (|| -> PackageResult {
        // SAFETY: `path_wide` is a live, NUL-terminated path buffer and all
        // output pointers refer to local storage owned for this call. The API
        // allocates `security_descriptor`, which is released below.
        let status = unsafe {
            GetNamedSecurityInfoW(
                path_wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
                &mut owner,
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut security_descriptor as *mut *mut std::ffi::c_void,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32).into());
        }

        // SAFETY: `GetCurrentProcess` returns the current process pseudo-handle
        // and `token` points to writable storage for the returned handle.
        let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
        if opened == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let mut required = 0_u32;
        // SAFETY: the null buffer intentionally performs the documented size
        // query; `required` is writable output storage.
        let _ = unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required)
        };
        if required == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let required_size = required as usize;
        if required_size < size_of::<TOKEN_USER>() {
            return Err("token information is shorter than TOKEN_USER".into());
        }
        let word_size = size_of::<usize>();
        let words = required_size
            .checked_add(word_size - 1)
            .ok_or_else(|| "token information size overflow".to_owned())?
            / word_size;
        if align_of::<usize>() < align_of::<TOKEN_USER>() {
            return Err("token information alignment is unsupported".into());
        }
        let mut token_buffer = vec![MaybeUninit::<usize>::uninit(); words];
        // SAFETY: the storage is aligned at least to `TOKEN_USER`, has at
        // least the requested byte length, and remains alive for the call.
        let queried = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                token_buffer.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        };
        if queried == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let returned_size = required as usize;
        if returned_size < size_of::<TOKEN_USER>()
            || returned_size > token_buffer.len().saturating_mul(word_size)
        {
            return Err("token information exceeds its storage".into());
        }
        // SAFETY: the successful query populated a complete TOKEN_USER at the
        // aligned beginning of the storage, and the storage is still alive.
        let token_user = unsafe {
            token_buffer
                .as_ptr()
                .cast::<TOKEN_USER>()
                .as_ref()
                .ok_or_else(|| "token information buffer is null".to_owned())?
        };
        if owner.is_null()
            || token_user.User.Sid.is_null()
            // SAFETY: both SID pointers come from successful Windows security
            // APIs and are non-null before they are validated here.
            || unsafe { IsValidSid(owner) == 0 || IsValidSid(token_user.User.Sid) == 0 }
        {
            return Err("private staging directory has an invalid owner SID".into());
        }
        // SAFETY: the two SIDs were checked for non-null and validity above.
        if unsafe { EqualSid(owner, token_user.User.Sid) == 0 } {
            return Err("private staging directory is not owned by the current user".into());
        }

        let mut control = SECURITY_DESCRIPTOR_CONTROL::default();
        let mut revision = 0_u32;
        // SAFETY: `security_descriptor` was allocated and populated by
        // `GetNamedSecurityInfoW` above; the output pointers are local.
        let descriptor_ok = unsafe {
            GetSecurityDescriptorControl(security_descriptor, &mut control, &mut revision)
        };
        if descriptor_ok == 0 || control & SE_DACL_PROTECTED == 0 {
            return Err("private staging directory DACL is inherited or unavailable".into());
        }
        let mut dacl_present = 0;
        let mut dacl_defaulted = 0;
        let mut checked_dacl = std::ptr::null_mut::<ACL>();
        // SAFETY: the descriptor and output pointers are live, and the API
        // writes only to the local DACL metadata.
        let dacl_ok = unsafe {
            GetSecurityDescriptorDacl(
                security_descriptor,
                &mut dacl_present,
                &mut checked_dacl,
                &mut dacl_defaulted,
            )
        };
        if dacl_ok == 0 || dacl_present == 0 || checked_dacl.is_null() || checked_dacl != dacl {
            return Err("private staging directory DACL is unavailable".into());
        }
        let mut size_information = ACL_SIZE_INFORMATION::default();
        // SAFETY: `checked_dacl` was returned by the successful DACL query and
        // `size_information` is writable local output storage.
        let acl_ok = unsafe {
            GetAclInformation(
                checked_dacl,
                (&mut size_information as *mut ACL_SIZE_INFORMATION).cast(),
                std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        };
        if acl_ok == 0 || size_information.AceCount != 2 {
            return Err("private staging directory DACL contains unexpected entries".into());
        }
        let acl_bytes = size_information.AclBytesInUse as usize;
        if acl_bytes < size_of::<ACL>() {
            return Err("private staging directory DACL size is invalid".into());
        }

        let system_name = wide_nul("S-1-5-18");
        // SAFETY: the static SID string is NUL-terminated and `system_sid` is
        // writable storage for the API-owned SID pointer.
        let converted = unsafe { ConvertStringSidToSidW(system_name.as_ptr(), &mut system_sid) };
        if converted == 0 || system_sid.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        let mut owner_ace = false;
        let mut system_ace = false;
        for index in 0..size_information.AceCount {
            let mut ace = std::ptr::null_mut::<std::ffi::c_void>();
            // SAFETY: `index` is bounded by the ACE count returned for the
            // validated ACL, and `ace` is writable output storage.
            if unsafe { GetAce(checked_dacl, index, &mut ace) == 0 } || ace.is_null() {
                return Err("private staging directory DACL ACE could not be read".into());
            }
            let acl_start = checked_dacl.cast::<u8>() as usize;
            let ace_start = ace.cast::<u8>() as usize;
            let ace_offset = ace_start.checked_sub(acl_start).ok_or_else(|| {
                "private staging directory DACL ACE is outside the ACL".to_owned()
            })?;
            let header_end = ace_offset
                .checked_add(size_of::<ACE_HEADER>())
                .ok_or_else(|| "private staging directory DACL ACE size overflow".to_owned())?;
            if header_end > acl_bytes {
                return Err("private staging directory DACL ACE header is truncated".into());
            }
            // SAFETY: the preceding range check covers the complete header;
            // `GetAce` may return an unaligned pointer, so read it unaligned.
            let header = unsafe { read_unaligned(ace.cast::<ACE_HEADER>()) };
            if header.AceType != 0 || header.AceFlags != 0 {
                return Err("private staging directory DACL contains a non-private ACE".into());
            }
            let ace_size = header.AceSize as usize;
            if ace_size < size_of::<ACCESS_ALLOWED_ACE>() {
                return Err("private staging directory DACL ACE is too short".into());
            }
            let ace_end = ace_offset
                .checked_add(ace_size)
                .ok_or_else(|| "private staging directory DACL ACE size overflow".to_owned())?;
            if ace_end > acl_bytes {
                return Err("private staging directory DACL ACE exceeds the ACL".into());
            }
            // SAFETY: the fixed ACCESS_ALLOWED_ACE prefix is fully covered by
            // the validated ACE range; use an unaligned copy rather than a
            // reference to API-provided bytes.
            let allowed = unsafe { read_unaligned(ace.cast::<ACCESS_ALLOWED_ACE>()) };
            if allowed.Mask == 0 {
                return Err("private staging directory DACL contains a zero-mask ACE".into());
            }
            let sid_offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
            let sid_end = sid_offset
                .checked_add(8)
                .ok_or_else(|| "private staging directory ACE SID offset overflow".to_owned())?;
            if sid_end > ace_size {
                return Err("private staging directory ACE SID header is truncated".into());
            }
            // SAFETY: `sid_offset` is inside the validated ACE range and the
            // SID pointer is used only after Windows validates it below.
            let ace_sid = unsafe { ace.cast::<u8>().add(sid_offset).cast() };
            // SAFETY: the SID header lies inside the validated ACE. The API
            // checks the SID shape before its length is used for bounds.
            if unsafe { IsValidSid(ace_sid) == 0 } {
                return Err("private staging directory DACL ACE has an invalid SID".into());
            }
            // SAFETY: `ace_sid` is a valid SID pointer according to the API,
            // and its reported length must still fit within this ACE.
            let sid_length = unsafe { GetLengthSid(ace_sid) } as usize;
            if sid_length == 0 || sid_length > ace_size - sid_offset {
                return Err("private staging directory DACL ACE SID exceeds the ACE".into());
            }
            // SAFETY: `ace_sid`, `owner`, and `system_sid` have all passed SID
            // validation before comparison.
            owner_ace |= unsafe { EqualSid(ace_sid, owner) != 0 };
            // SAFETY: `ace_sid` and `system_sid` have both passed SID validation
            // before comparison.
            system_ace |= unsafe { EqualSid(ace_sid, system_sid) != 0 };
        }
        if !owner_ace || !system_ace {
            return Err("private staging directory DACL does not match the private policy".into());
        }
        Ok(())
    })();

    // SAFETY: each pointer is either null or owned by the corresponding
    // Windows API call above, and each resource is released at most once.
    unsafe {
        if !token.is_null() {
            let _ = CloseHandle(token);
        }
        if !system_sid.is_null() {
            let _ = LocalFree(system_sid as HLOCAL);
        }
        if !security_descriptor.is_null() {
            let _ = LocalFree(security_descriptor as HLOCAL);
        }
    }
    result
}

#[allow(clippy::permissions_set_readonly_false)]
fn manifest_permissions() -> PackageResult<std::fs::Permissions> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        Ok(std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let temporary = tempfile::Builder::new()
            .prefix(".xlfn-manifest-permissions-")
            .tempfile()?;
        let mut permissions = temporary.as_file().metadata()?.permissions();
        permissions.set_readonly(false);
        Ok(permissions)
    }
}

fn map_snapshot_open_error(target: &str, path: &Path, error: io::Error) -> PackageError {
    #[cfg(target_os = "windows")]
    {
        use crate::win32::ERROR_SHARING_VIOLATION;

        if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) {
            return PackageError::BundleSourceBusy {
                target: target.to_owned(),
                path: path.to_owned(),
                source: error,
            };
        }
    }

    PackageError::Message(format!(
        "{target}: failed to open {}: {error}",
        path.display()
    ))
}

fn unstable_bundle_source(target: &str, path: &Path) -> PackageError {
    PackageError::UnstableBundleSource {
        target: target.to_owned(),
        path: path.to_owned(),
    }
}

fn read_stable_snapshot(
    target: &str,
    path: &Path,
    file: &mut std::fs::File,
    observer: &dyn SnapshotObserver,
) -> PackageResult<Arc<[u8]>> {
    read_stable_snapshot_with_limit(target, path, file, observer, None)
}

fn read_stable_snapshot_with_limit(
    target: &str,
    path: &Path,
    file: &mut std::fs::File,
    observer: &dyn SnapshotObserver,
    maximum_len: Option<u64>,
) -> PackageResult<Arc<[u8]>> {
    let before = file_snapshot_state(file)?;
    if maximum_len.is_some_and(|maximum| before.len > maximum) {
        return Err(PackageError::Message(format!(
            "{target}: file exceeds the snapshot byte budget: {}",
            path.display()
        )));
    }
    let expected_len = usize::try_from(before.len).map_err(|_| {
        PackageError::Message(format!(
            "{target}: bundle file is too large to snapshot: {}",
            path.display()
        ))
    })?;

    let mut snapshot = Vec::new();
    snapshot.try_reserve_exact(expected_len).map_err(|_| {
        PackageError::Message(format!(
            "{target}: cannot reserve {expected_len} bytes for bundle snapshot: {}",
            path.display()
        ))
    })?;

    file.seek(SeekFrom::Start(0))?;
    let mut limited = file.take(before.len.saturating_add(1));
    let mut first_chunk = [0_u8; 64 * 1024];
    let count = limited.read(&mut first_chunk)?;
    if count != 0 {
        observer.after_first_chunk(path);
        if count > expected_len {
            return Err(unstable_bundle_source(target, path));
        }
        snapshot.extend_from_slice(&first_chunk[..count]);
    }

    let remaining = expected_len
        .checked_sub(snapshot.len())
        .ok_or_else(|| unstable_bundle_source(target, path))?;
    limited
        .by_ref()
        .take(remaining as u64)
        .read_to_end(&mut snapshot)?;
    if snapshot.len() != expected_len {
        return Err(unstable_bundle_source(target, path));
    }

    let mut extra = [0_u8; 1];
    if limited.read(&mut extra)? != 0 {
        return Err(unstable_bundle_source(target, path));
    }

    let after = file_snapshot_state(file)?;
    if before != after {
        return Err(unstable_bundle_source(target, path));
    }

    Ok(Arc::from(snapshot))
}

#[cfg(not(target_os = "windows"))]
fn verify_snapshot_against_second_read(
    target: &str,
    path: &Path,
    file: &mut std::fs::File,
    expected: &[u8],
) -> PackageResult {
    let before = file_snapshot_state(file)?;
    file.seek(SeekFrom::Start(0))?;

    let expected_digest = Sha256::digest(expected);
    let mut hasher = Sha256::new();
    let mut limited = file.take(before.len.saturating_add(1));
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let count = limited.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    let observed_digest = hasher.finalize();
    let after = file_snapshot_state(file)?;
    if before != after || expected_digest != observed_digest {
        return Err(unstable_bundle_source(target, path));
    }

    Ok(())
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    volume_serial_number: u64,
    file_id: [u8; 16],
}

#[cfg(not(any(unix, target_os = "windows")))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileSnapshotState {
    identity: FileIdentity,
    len: u64,
    #[cfg(unix)]
    mtime: i64,
    #[cfg(unix)]
    mtime_nsec: i64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
    #[cfg(target_os = "windows")]
    last_write_time: u64,
}

#[cfg(unix)]
fn file_snapshot_state(file: &std::fs::File) -> io::Result<FileSnapshotState> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata()?;
    Ok(FileSnapshotState {
        identity: FileIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        },
        len: metadata.len(),
        mtime: metadata.mtime(),
        mtime_nsec: metadata.mtime_nsec(),
        ctime: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
    })
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
fn file_snapshot_state(file: &std::fs::File) -> io::Result<FileSnapshotState> {
    use crate::win32::{
        BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx, HANDLE,
    };
    use std::os::windows::io::AsRawHandle;

    let handle = file.as_raw_handle() as HANDLE;
    // `BY_HANDLE_FILE_INFORMATION` exposes only a 64-bit file index. ReFS can
    // use 128-bit file IDs, so identity must come from `FILE_ID_INFO`.
    let mut identity = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: `handle` remains valid for the duration of the call because it is borrowed
    // from `file`, and `identity` points to writable storage of the required type.
    let status = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            identity.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if status == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful `GetFileInformationByHandleEx` initializes the complete
    // `FILE_ID_INFO` output.
    let identity = unsafe { identity.assume_init() };

    let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: `handle` remains valid for the duration of the call because it is borrowed
    // from `file`, and `information` points to writable storage of the required type.
    let status = unsafe { GetFileInformationByHandle(handle, information.as_mut_ptr()) };
    if status == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful `GetFileInformationByHandle` initializes the complete
    // `BY_HANDLE_FILE_INFORMATION` output structure.
    let information = unsafe { information.assume_init() };

    let len = (u64::from(information.nFileSizeHigh) << 32) | u64::from(information.nFileSizeLow);
    let last_write_time = (u64::from(information.ftLastWriteTime.dwHighDateTime) << 32)
        | u64::from(information.ftLastWriteTime.dwLowDateTime);

    Ok(FileSnapshotState {
        identity: FileIdentity {
            volume_serial_number: identity.VolumeSerialNumber,
            file_id: identity.FileId.Identifier,
        },
        len,
        last_write_time,
    })
}

#[cfg(not(any(unix, target_os = "windows")))]
fn file_snapshot_state(file: &std::fs::File) -> io::Result<FileSnapshotState> {
    Ok(FileSnapshotState {
        identity: FileIdentity,
        len: file.metadata()?.len(),
    })
}

#[cfg(unix)]
fn same_file_identity(left: &std::fs::File, right: &std::fs::File) -> io::Result<bool> {
    Ok(file_snapshot_state(left)?.identity == file_snapshot_state(right)?.identity)
}

#[cfg(target_os = "windows")]
fn same_file_identity(left: &std::fs::File, right: &std::fs::File) -> io::Result<bool> {
    Ok(file_snapshot_state(left)?.identity == file_snapshot_state(right)?.identity)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn same_file_identity(left: &std::fs::File, right: &std::fs::File) -> io::Result<bool> {
    // The supported release hosts are Unix and Windows. Keep other targets
    // conservative rather than claiming identity from path metadata alone.
    let _ = (left, right);
    Ok(false)
}

fn component_eq(left: Component<'_>, right: Component<'_>) -> bool {
    // Both paths have already been canonicalized before this helper is used.
    // Comparing the native components directly avoids lossy UTF-16 decoding
    // and the incorrect ASCII-only case folding previously used here.
    left.as_os_str() == right.as_os_str()
}

fn reject_reparse_points(root: &Path, configured_path: &str) -> PackageResult {
    let mut current = root.to_path_buf();
    for component in Path::new(configured_path).components() {
        let Component::Normal(component) = component else {
            return Err(format!("bundle file has unsafe path {configured_path:?}").into());
        };
        current.push(component);
        if let Ok(metadata) = fs::symlink_metadata(&current)
            && is_reparse_point(&metadata)
        {
            return Err(format!(
                "strict bundle path rejects symlink or reparse point: {}",
                current.display()
            )
            .into());
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(target_os = "windows"))]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}
fn verify_machine(info: &PeInfo, architecture: Architecture, path: &Path) -> PackageResult {
    if info.machine == architecture.pe_machine() {
        Ok(())
    } else {
        Err(format!("{} has wrong PE machine", path.display()).into())
    }
}
fn verify_image_characteristics(info: &PeInfo, path: &Path) -> PackageResult {
    if info.characteristics & IMAGE_FILE_EXECUTABLE_IMAGE == object::pe::FileFlags::default() {
        return Err(format!("{} is not an executable PE image", path.display()).into());
    }
    if info.characteristics & IMAGE_FILE_DLL == object::pe::FileFlags::default() {
        return Err(format!("{} is not marked as a PE DLL", path.display()).into());
    }
    if info.characteristics & IMAGE_FILE_SYSTEM != object::pe::FileFlags::default() {
        return Err(format!("{} is marked as a system image", path.display()).into());
    }
    Ok(())
}

fn reject_forwarded_xll_export(info: &PeInfo, path: &Path, export: &str) -> PackageResult {
    let symbol = ExportSymbol::Name(export.to_owned());
    if let Some(forwarded) = info.forwarded_exports.get(&symbol) {
        return Err(format!(
            "{} forwards required XLL export {export} to {}!{}; XLL entry points must be implemented directly",
            path.display(),
            forwarded.library,
            format_export_symbol(&forwarded.symbol)
        )
        .into());
    }
    Ok(())
}
fn is_system(name: &str) -> bool {
    const SYSTEM_DLLS: &[&str] = &[
        "advapi32.dll",
        "api-ms-win-core-synch-l1-1-0.dll",
        "api-ms-win-core-synch-l1-2-0.dll",
        "bcrypt.dll",
        "bcryptprimitives.dll",
        "cabinet.dll",
        "cfgmgr32.dll",
        "comctl32.dll",
        "comdlg32.dll",
        "combase.dll",
        "crypt32.dll",
        "d2d1.dll",
        "d3d11.dll",
        "dwrite.dll",
        "dwmapi.dll",
        "dxgi.dll",
        "gdi32.dll",
        "imm32.dll",
        "iphlpapi.dll",
        "kernel32.dll",
        "mpr.dll",
        "msvcrt.dll",
        "netapi32.dll",
        "ntdll.dll",
        "ole32.dll",
        "oleaut32.dll",
        "powrprof.dll",
        "psapi.dll",
        "rpcrt4.dll",
        "secur32.dll",
        "setupapi.dll",
        "shell32.dll",
        "shlwapi.dll",
        "ucrtbase.dll",
        "user32.dll",
        "userenv.dll",
        "uxtheme.dll",
        "version.dll",
        "winhttp.dll",
        "wininet.dll",
        "winmm.dll",
        "wintrust.dll",
        "ws2_32.dll",
    ];

    SYSTEM_DLLS.contains(&name)
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ExportSymbol {
    Name(String),
    Ordinal(ExportOrdinal),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForwardedExport {
    pub library: String,
    pub symbol: ExportSymbol,
}

#[derive(Clone, Debug)]
pub struct PeInfo {
    pub machine: object::pe::Machine,
    pub characteristics: object::pe::FileFlags,
    pub exports: BTreeSet<String>,
    pub forwarded_exports: BTreeMap<ExportSymbol, ForwardedExport>,
    pub executable_exports: BTreeSet<String>,
    pub has_export_manifest: bool,
    pub expected_exports: BTreeSet<String>,
    pub crt_policy: Option<EffectiveCrtPolicy>,
    pub imports: BTreeSet<String>,
    pub import_targets: BTreeMap<String, BTreeSet<ImportTarget>>,
    pub delay_imports: BTreeSet<String>,
    pub delay_import_targets: BTreeMap<String, BTreeSet<ImportTarget>>,
    /// Ordinals represented by non-zero export address table entries.
    pub exported_ordinals: BTreeSet<ExportOrdinal>,
    /// Non-zero export address table indices, used for closed-world validation.
    pub nonzero_export_slots: BTreeSet<ExportAddressIndex>,
    /// Export address table indices referenced by at least one export name.
    pub named_export_slots: BTreeSet<ExportAddressIndex>,
}

impl Default for PeInfo {
    fn default() -> Self {
        Self {
            machine: IMAGE_FILE_MACHINE_AMD64,
            characteristics: object::pe::FileFlags::default(),
            exports: BTreeSet::new(),
            forwarded_exports: BTreeMap::new(),
            executable_exports: BTreeSet::new(),
            has_export_manifest: true,
            expected_exports: BTreeSet::new(),
            crt_policy: None,
            imports: BTreeSet::new(),
            import_targets: BTreeMap::new(),
            delay_imports: BTreeSet::new(),
            delay_import_targets: BTreeMap::new(),
            exported_ordinals: BTreeSet::new(),
            nonzero_export_slots: BTreeSet::new(),
            named_export_slots: BTreeSet::new(),
        }
    }
}

impl PeInfo {
    fn has_export_symbol(&self, symbol: &ExportSymbol) -> bool {
        match symbol {
            ExportSymbol::Name(name) => self.exports.contains(name),
            ExportSymbol::Ordinal(ordinal) => self.exported_ordinals.contains(ordinal),
        }
    }
}
pub fn inspect_pe(path: &Path) -> PackageResult<PeInfo> {
    parse_pe_bytes(&fs::read(path)?)
}

pub fn parse_pe_bytes(b: &[u8]) -> PackageResult<PeInfo> {
    match FileKind::parse(b)? {
        FileKind::Pe32 => parse_pe_file(PeFile32::parse(b)?),
        FileKind::Pe64 => parse_pe_file(PeFile64::parse(b)?),
        _ => Err("file is not a PE32 or PE32+ image".into()),
    }
}

fn normalize_forwarder_library(library: &str) -> PackageResult<String> {
    validate_relative("forwarded export library", library)?;
    let path = Path::new(library);
    if path.file_name().and_then(|value| value.to_str()) != Some(library) {
        return Err(format!("forwarded export library must be a basename, got {library:?}").into());
    }
    let mut normalized = library.to_ascii_lowercase();
    if !normalized.contains('.') {
        normalized.push_str(".dll");
    } else if !normalized.ends_with(".dll") && !normalized.ends_with(".xll") {
        return Err(format!("forwarded export library has invalid name {library:?}").into());
    }
    Ok(normalized)
}

fn parse_pe_file<Pe>(pe: PeFile<'_, Pe>) -> PackageResult<PeInfo>
where
    Pe: ImageNtHeaders,
{
    let file_header = pe.nt_headers().file_header();
    let machine = file_header.machine.get(LE);
    let characteristics = file_header.characteristics.get(LE);
    let mut exports = BTreeSet::new();
    let mut executable_exports = BTreeSet::new();
    let mut forwarded_exports = BTreeMap::new();
    let mut exported_ordinals = BTreeSet::new();
    let mut nonzero_export_slots = BTreeSet::new();
    let mut named_export_slots = BTreeSet::new();
    if let Some(table) = pe.export_table()? {
        let mut export_targets = BTreeMap::new();
        for (ordinal_index, exported_ordinal, address) in table.address_iter() {
            if address == 0 {
                export_targets.insert(ordinal_index, None);
                continue;
            }

            nonzero_export_slots.insert(ordinal_index);
            exported_ordinals.insert(exported_ordinal);
            let executable = match table.target_from_address(address)? {
                ExportTarget::Address(address) => {
                    let section = pe
                        .section_table()
                        .iter()
                        .find(|section| {
                            let start = section.virtual_address.get(LE);
                            let size = section
                                .virtual_size
                                .get(LE)
                                .max(section.size_of_raw_data.get(LE));
                            address >= start && address < start.saturating_add(size)
                        })
                        .ok_or_else(|| {
                            PackageError::Message(format!(
                                "PE export ordinal {exported_ordinal} points outside every mapped section"
                            ))
                        })?;
                    (section.characteristics.get(LE) & IMAGE_SCN_MEM_EXECUTE)
                        != object::pe::SectionFlags::default()
                }
                ExportTarget::ForwardByOrdinal(library, target_ordinal) => {
                    let forwarded = ForwardedExport {
                        library: normalize_forwarder_library(std::str::from_utf8(library)?)?,
                        symbol: ExportSymbol::Ordinal(target_ordinal),
                    };
                    forwarded_exports.insert(ExportSymbol::Ordinal(exported_ordinal), forwarded);
                    false
                }
                ExportTarget::ForwardByName(library, target_name) => {
                    let forwarded = ForwardedExport {
                        library: normalize_forwarder_library(std::str::from_utf8(library)?)?,
                        symbol: ExportSymbol::Name(std::str::from_utf8(target_name)?.to_owned()),
                    };
                    forwarded_exports.insert(ExportSymbol::Ordinal(exported_ordinal), forwarded);
                    false
                }
            };

            export_targets.insert(ordinal_index, Some(executable));
        }

        for (name_pointer, ordinal_index) in table.name_iter() {
            named_export_slots.insert(ordinal_index);
            let target = export_targets
                .get(&ordinal_index)
                .copied()
                .ok_or_else(|| PackageError::Message("invalid PE export ordinal index".into()))?;
            let Some(executable) = target else {
                // A name attached to a zero EAT entry is not a resolvable
                // export and must not satisfy lifecycle or import validation.
                continue;
            };
            let name = std::str::from_utf8(table.name_from_pointer(name_pointer)?)?.to_owned();
            if executable {
                executable_exports.insert(name.clone());
            }
            exports.insert(name);
        }
    }

    let mut has_export_manifest = false;
    let mut expected_exports = BTreeSet::new();
    let mut crt_policy = None;
    for section in pe.section_table().iter() {
        let name_str = std::str::from_utf8(&section.name)
            .unwrap_or("")
            .trim_matches('\0');
        let is_export_manifest = name_str == ".xllexp" || name_str.ends_with(".xllexp");
        if is_export_manifest {
            has_export_manifest = true;
        }
        if is_export_manifest && let Ok(data) = section.pe_data(pe.data()) {
            for part in data.split(|&b| b == 0) {
                if !part.is_empty()
                    && let Ok(export_name) = std::str::from_utf8(part)
                {
                    let trimmed = export_name.trim();
                    if !trimmed.is_empty() {
                        expected_exports.insert(trimmed.to_owned());
                    }
                }
            }
        }
        if name_str == ".xlfncrt" || name_str.ends_with(".xlfncrt") {
            let data = section.pe_data(pe.data()).map_err(|error| {
                PackageError::Message(format!("failed to read .xlfncrt section: {error}"))
            })?;
            let observed = parse_crt_marker(data)?;
            if crt_policy.replace(observed).is_some() {
                return Err("PE image contains multiple .xlfncrt markers".into());
            }
        }
    }

    let mut imports = BTreeSet::new();
    let mut import_targets: BTreeMap<String, BTreeSet<ImportTarget>> = BTreeMap::new();
    if let Some(table) = pe.import_table()? {
        let mut descriptors = table.descriptors()?;
        while let Some(descriptor) = descriptors.next()? {
            let name = std::str::from_utf8(table.name(descriptor.name.get(LE))?)?.to_owned();
            let lookup_rva = {
                let original = descriptor.original_first_thunk.get(LE);
                if original == 0 {
                    descriptor.first_thunk.get(LE)
                } else {
                    original
                }
            };
            let mut targets = BTreeSet::new();
            if lookup_rva != 0 {
                let mut thunks = table.thunks(lookup_rva)?;
                while let Some(thunk) = thunks.next::<Pe>()? {
                    match table.import::<Pe>(thunk)? {
                        PeImport::Ordinal(ordinal) => {
                            targets.insert(ImportTarget::Ordinal(ordinal));
                        }
                        PeImport::Name(_, symbol) => {
                            targets.insert(ImportTarget::Name(
                                std::str::from_utf8(symbol)?.to_owned(),
                            ));
                        }
                    }
                }
            }
            imports.insert(name.clone());
            import_targets.entry(name).or_default().extend(targets);
        }
    }

    let mut delay_imports = BTreeSet::new();
    let mut delay_import_targets: BTreeMap<String, BTreeSet<ImportTarget>> = BTreeMap::new();
    if let Some(table) = pe
        .data_directories()
        .delay_load_import_table(pe.data(), &pe.section_table())?
    {
        let mut descriptors = table.descriptors()?;
        while let Some(descriptor) = descriptors.next()? {
            if descriptor.attributes.get(LE) & 1 == 0 {
                return Err("unsupported VA-based delay load import descriptor".into());
            }
            let name =
                std::str::from_utf8(table.name(descriptor.dll_name_rva.get(LE))?)?.to_owned();
            let lookup_rva = descriptor.import_name_table_rva.get(LE);
            if lookup_rva == 0 {
                return Err(format!("delay import {name:?} has no import name table").into());
            }
            let mut targets = BTreeSet::new();
            let mut thunks = table.thunks(lookup_rva)?;
            while let Some(thunk) = thunks.next::<Pe>()? {
                match table.import::<Pe>(thunk)? {
                    PeImport::Ordinal(ordinal) => {
                        targets.insert(ImportTarget::Ordinal(ordinal));
                    }
                    PeImport::Name(_, symbol) => {
                        targets.insert(ImportTarget::Name(std::str::from_utf8(symbol)?.to_owned()));
                    }
                }
            }
            delay_imports.insert(name.clone());
            delay_import_targets
                .entry(name)
                .or_default()
                .extend(targets);
        }
    }

    Ok(PeInfo {
        machine,
        characteristics,
        exports,
        forwarded_exports,
        executable_exports,
        has_export_manifest,
        expected_exports,
        crt_policy,
        imports,
        import_targets,
        delay_imports,
        delay_import_targets,
        exported_ordinals,
        nonzero_export_slots,
        named_export_slots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn xll_info() -> PeInfo {
        let framework = REQUIRED_XLL_EXPORTS
            .iter()
            .copied()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        PeInfo {
            machine: IMAGE_FILE_MACHINE_AMD64,
            characteristics: IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL,
            exports: framework.clone(),
            forwarded_exports: BTreeMap::new(),
            executable_exports: framework.clone(),
            has_export_manifest: true,
            expected_exports: framework,
            crt_policy: Some(EffectiveCrtPolicy::Dynamic),
            imports: BTreeSet::new(),
            import_targets: BTreeMap::new(),
            delay_imports: BTreeSet::new(),
            delay_import_targets: BTreeMap::new(),
            exported_ordinals: BTreeSet::new(),
            nonzero_export_slots: BTreeSet::new(),
            named_export_slots: BTreeSet::new(),
        }
    }

    #[test]
    fn crt_marker_records_the_effective_compiler_policy() {
        let mut dynamic = [0_u8; 16];
        dynamic[..8].copy_from_slice(CRT_MARKER_MAGIC);
        dynamic[8] = CRT_MARKER_SCHEMA;
        assert_eq!(
            parse_crt_marker(&dynamic).unwrap(),
            EffectiveCrtPolicy::Dynamic
        );

        dynamic[9] = 1;
        assert_eq!(
            parse_crt_marker(&dynamic).unwrap(),
            EffectiveCrtPolicy::Static
        );
        dynamic[8] = 2;
        assert!(parse_crt_marker(&dynamic).is_err());
    }

    #[test]
    fn crt_marker_requires_a_canonical_section_layout() {
        let mut marker = [0_u8; 16];
        marker[..8].copy_from_slice(CRT_MARKER_MAGIC);
        marker[8] = CRT_MARKER_SCHEMA;

        let mut junk_prefix = vec![0x5a];
        junk_prefix.extend_from_slice(&marker);
        assert!(parse_crt_marker(&junk_prefix).is_err());

        let mut duplicate = marker.to_vec();
        duplicate.extend_from_slice(&marker);
        assert!(parse_crt_marker(&duplicate).is_err());

        let mut reserved = marker;
        reserved[10] = 1;
        assert!(parse_crt_marker(&reserved).is_err());

        let mut padding = marker.to_vec();
        padding.extend_from_slice(&[0; 8]);
        assert_eq!(
            parse_crt_marker(&padding).unwrap(),
            EffectiveCrtPolicy::Dynamic
        );
    }

    #[test]
    fn xll_verification_requires_the_framework_manifest_and_lifecycle_exports() {
        let path = Path::new("addin.xll");
        let mut missing_manifest = xll_info();
        missing_manifest.has_export_manifest = false;
        assert!(
            verify_xll_exports(&missing_manifest, path, &[])
                .unwrap_err()
                .to_string()
                .contains(".xllexp")
        );

        let mut missing_crt_marker = xll_info();
        missing_crt_marker.crt_policy = None;
        assert!(
            verify_xll_exports(&missing_crt_marker, path, &[])
                .unwrap_err()
                .to_string()
                .contains(".xlfncrt")
        );

        let mut missing_lifecycle = xll_info();
        missing_lifecycle.exports.remove("xlAutoOpen");
        assert!(
            verify_xll_exports(&missing_lifecycle, path, &[])
                .unwrap_err()
                .to_string()
                .contains("xlAutoOpen")
        );

        assert!(verify_xll_exports(&xll_info(), path, &[]).is_ok());
    }

    #[test]
    fn xll_verification_reconciles_manifest_and_actual_udf_exports() {
        let path = Path::new("addin.xll");
        let mut info = xll_info();
        info.expected_exports.insert("xll_compute".to_owned());
        assert!(
            verify_xll_exports(&info, path, &[])
                .unwrap_err()
                .to_string()
                .contains("xll_compute")
        );
        info.exports.insert("xll_compute".to_owned());
        info.executable_exports.insert("xll_compute".to_owned());
        assert!(verify_xll_exports(&info, path, &["xll_compute".to_owned()]).is_ok());
    }

    #[test]
    fn xll_verification_rejects_forwarded_entry_points() {
        let path = Path::new("addin.xll");
        let mut info = xll_info();
        let forwarded = ForwardedExport {
            library: "helper.dll".to_owned(),
            symbol: ExportSymbol::Name("Open".to_owned()),
        };
        let _ = info
            .forwarded_exports
            .insert(ExportSymbol::Name("xlAutoOpen".to_owned()), forwarded);
        let error = verify_xll_exports(&info, path, &[]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("forwards required XLL export xlAutoOpen")
        );
    }

    #[test]
    fn pe_image_characteristics_require_an_executable_dll() {
        let path = Path::new("addin.xll");
        let mut info = xll_info();
        info.characteristics = IMAGE_FILE_EXECUTABLE_IMAGE;
        assert!(verify_image_characteristics(&info, path).is_err());
        info.characteristics = IMAGE_FILE_DLL;
        assert!(verify_image_characteristics(&info, path).is_err());
        info.characteristics = IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL | IMAGE_FILE_SYSTEM;
        assert!(verify_image_characteristics(&info, path).is_err());
        info.characteristics = IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL;
        assert!(verify_image_characteristics(&info, path).is_ok());
    }

    #[test]
    fn xll_verification_rejects_named_exports_without_executable_targets() {
        let path = Path::new("addin.xll");
        let mut info = xll_info();
        info.executable_exports.remove("xlAutoOpen");
        let error = verify_xll_exports(&info, path, &[]).unwrap_err();
        assert!(error.to_string().contains("direct executable target"));
    }

    #[test]
    fn unsafe_bundle_paths_are_rejected() {
        assert!(validate_relative("file", "../Vendor.dll").is_err());
        assert!(validate_relative("file", "/Vendor.dll").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn directory_validation_checks_existing_ancestors() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let real_parent = directory.path().join("real-parent");
        let linked_parent = directory.path().join("linked-parent");
        fs::create_dir(&real_parent).unwrap();
        symlink(&real_parent, &linked_parent).unwrap();

        let error =
            validate_directory_path(&linked_parent.join("nested").join("dist")).unwrap_err();
        assert!(error.to_string().contains("symlink or reparse point"));
    }

    #[test]
    fn bounded_directory_entry_read_stops_before_collecting_extra_entries() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("first"), []).unwrap();
        fs::write(directory.path().join("second"), []).unwrap();

        let error = read_directory_entries(directory.path(), 1).unwrap_err();
        assert!(error.to_string().contains("entry budget"));
    }

    #[test]
    fn bundle_metadata_rejects_unknown_fields() {
        let parsed: BundleMetadata =
            serde_json::from_str(
                r#"{"x86":["native/x86/A.dll"],"x64":["native/x64/A.dll"],"external-imports":["Inbox.dll"],"strict-paths":true}"#,
            )
            .unwrap();
        assert_eq!(parsed.x86.len(), 1);
        assert_eq!(parsed.external_imports, vec!["Inbox.dll"]);
        assert!(parsed.strict_paths);
        assert!(serde_json::from_str::<BundleMetadata>(r#"{"unknown":true}"#).is_err());
    }

    #[test]
    fn bundle_metadata_and_simple_resolution_default_to_strict_paths() {
        let parsed: BundleMetadata = serde_json::from_str(r#"{"x86":[],"x64":[]}"#).unwrap();
        assert!(parsed.strict_paths);
        assert!(BundleMetadata::default().strict_paths);

        let manifest = tempfile::tempdir().unwrap();
        fs::write(manifest.path().join("Engine.dll"), []).unwrap();
        let bundle = resolve_bundle_files(
            manifest.path(),
            "x86_64-pc-windows-msvc",
            &["Engine.dll".to_owned()],
        )
        .unwrap();
        assert_eq!(bundle.resolved_files().count(), 1);
    }

    #[test]
    fn case_insensitive_bundle_basenames_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("one")).unwrap();
        fs::create_dir_all(directory.path().join("two")).unwrap();
        fs::write(directory.path().join("one/Engine.dll"), []).unwrap();
        fs::write(directory.path().join("two/engine.DLL"), []).unwrap();
        let error = resolve_bundle_files(
            directory.path(),
            "x86_64-pc-windows-msvc",
            &["one/Engine.dll".to_owned(), "two/engine.DLL".to_owned()],
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("duplicate bundle basename"));
    }

    #[test]
    fn windows_invalid_and_non_ascii_dll_basenames_are_rejected_on_every_host() {
        for name in [
            "CON.dll",
            "aux.DLL",
            "COM1.dll",
            "LPT9.dll",
            "trailing.dll.",
            "trailing.dll ",
            "foo:bar.dll",
            r"foo\bar.dll",
            "Ä.dll",
        ] {
            assert!(
                windows_dll_name_key("bundle file", name).is_err(),
                "{name:?} unexpectedly passed Windows basename validation"
            );
        }
        assert_eq!(
            windows_dll_name_key("bundle file", "Engine.DLL").unwrap(),
            "engine.dll"
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn bundle_resolution_applies_windows_basename_rules_on_other_hosts() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("CON.dll"), []).unwrap();

        let error = resolve_bundle_files(
            directory.path(),
            "x86_64-pc-windows-msvc",
            &["CON.dll".to_owned()],
        )
        .unwrap_err();
        assert!(error.to_string().contains("reserved Windows device name"));
    }

    #[test]
    fn staging_flattens_multiple_configured_files() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        fs::write(source.path().join("Engine.dll"), b"engine").unwrap();
        fs::write(source.path().join("Math.dll"), b"math").unwrap();
        let bundle = resolve_bundle_files(
            source.path(),
            "x86_64-pc-windows-msvc",
            &["Engine.dll".to_owned(), "Math.dll".to_owned()],
        )
        .unwrap();
        let staging_dir = destination.path().join("stage");
        let staging_directory = PrivateStagingDirectory::create(&staging_dir).unwrap();
        let staged = stage_bundle(&bundle, &staging_directory).unwrap();
        assert_eq!(staged.files.len(), 2);
        assert!(
            staged
                .files
                .iter()
                .all(|file| file.source.starts_with(&staging_dir))
        );
        fs::write(source.path().join("Engine.dll"), b"replaced").unwrap();
        assert_eq!(fs::read(staging_dir.join("Engine.dll")).unwrap(), b"engine");
        assert_eq!(fs::read(staging_dir.join("Math.dll")).unwrap(), b"math");
    }

    #[test]
    fn arbitrary_bytes_never_panic_the_pe_parser() {
        for length in 0..512 {
            let bytes = (0..length)
                .map(|index| ((index * 31 + length * 17) & 0xff) as u8)
                .collect::<Vec<_>>();
            let result = std::panic::catch_unwind(|| parse_pe_bytes(&bytes));
            assert!(result.is_ok(), "parser panicked for {length} bytes");
        }
    }

    fn write_u16<T>(destination: &mut [u8], value: T)
    where
        T: object::Wrap<Inner = u16> + Copy + 'static,
    {
        let encoded = object::endian::U16::<LE, T>::new(LE, value);
        destination.copy_from_slice(object::pod::bytes_of(&encoded));
    }

    fn minimal_pe(machine: object::pe::Machine, characteristics: object::pe::FileFlags) -> Vec<u8> {
        let peoff = 0x80usize;
        let raw = 0x200usize;
        let mut buf = vec![0_u8; 0x400];
        buf[0..2].copy_from_slice(b"MZ");
        buf[0x3c..0x40].copy_from_slice(&(peoff as u32).to_le_bytes());
        let mut offset = peoff;
        buf[offset..offset + 4].copy_from_slice(b"PE\0\0");
        offset += 4;
        write_u16(&mut buf[offset..offset + 2], machine);
        buf[offset + 2..offset + 4].copy_from_slice(&1_u16.to_le_bytes());
        buf[offset + 16..offset + 18].copy_from_slice(&0x00f0_u16.to_le_bytes());
        write_u16(&mut buf[offset + 18..offset + 20], characteristics);
        offset += 20;
        let optional = offset;
        buf[optional..optional + 2].copy_from_slice(&0x20b_u16.to_le_bytes());
        buf[optional + 2] = 14;
        buf[optional + 8..optional + 12].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[optional + 20..optional + 24].copy_from_slice(&0x1000_u32.to_le_bytes());
        buf[optional + 24..optional + 32].copy_from_slice(&0x140000000_u64.to_le_bytes());
        buf[optional + 32..optional + 36].copy_from_slice(&0x1000_u32.to_le_bytes());
        buf[optional + 36..optional + 40].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[optional + 40..optional + 42].copy_from_slice(&6_u16.to_le_bytes());
        buf[optional + 48..optional + 50].copy_from_slice(&6_u16.to_le_bytes());
        buf[optional + 56..optional + 60].copy_from_slice(&0x2000_u32.to_le_bytes());
        buf[optional + 60..optional + 64].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[optional + 68..optional + 70].copy_from_slice(&2_u16.to_le_bytes());
        buf[optional + 70..optional + 72].copy_from_slice(&0x8160_u16.to_le_bytes());
        buf[optional + 72..optional + 80].copy_from_slice(&0x100000_u64.to_le_bytes());
        buf[optional + 80..optional + 88].copy_from_slice(&0x1000_u64.to_le_bytes());
        buf[optional + 88..optional + 96].copy_from_slice(&0x100000_u64.to_le_bytes());
        buf[optional + 96..optional + 104].copy_from_slice(&0x1000_u64.to_le_bytes());
        buf[optional + 108..optional + 112].copy_from_slice(&16_u32.to_le_bytes());

        offset = optional + 0xf0;
        buf[offset..offset + 8].copy_from_slice(b".text\0\0\0");
        buf[offset + 8..offset + 12].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[offset + 12..offset + 16].copy_from_slice(&0x1000_u32.to_le_bytes());
        buf[offset + 16..offset + 20].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[offset + 20..offset + 24].copy_from_slice(&(raw as u32).to_le_bytes());
        buf[offset + 32..offset + 36].copy_from_slice(&0x60000020_u32.to_le_bytes());
        buf
    }

    #[test]
    fn verify_bundle_files_checks_dll_characteristics() {
        let cases = [
            (IMAGE_FILE_DLL, "not an executable PE image"),
            (IMAGE_FILE_EXECUTABLE_IMAGE, "not marked as a PE DLL"),
            (
                IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL | IMAGE_FILE_SYSTEM,
                "marked as a system image",
            ),
        ];
        for (index, (characteristics, expected)) in cases.into_iter().enumerate() {
            let directory = tempfile::tempdir().unwrap();
            let name = format!("Engine{index}.dll");
            fs::write(
                directory.path().join(&name),
                minimal_pe(IMAGE_FILE_MACHINE_AMD64, characteristics),
            )
            .unwrap();
            let bundle = resolve_bundle_files(
                directory.path(),
                "x86_64-pc-windows-msvc",
                std::slice::from_ref(&name),
            )
            .unwrap();
            let error = verify_bundle_files(&bundle, "x86_64-pc-windows-msvc").unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    enum SyntheticExportTarget<'a> {
        Zero,
        Direct,
        ForwardedOrdinal(&'a str, u32),
    }

    fn synthetic_export_pe(
        ordinal_base: u32,
        targets: &[SyntheticExportTarget<'_>],
        names: &[(usize, &str)],
    ) -> Vec<u8> {
        let peoff = 0x80usize;
        let edata_raw = 0x200usize;
        let text_raw = 0x400usize;
        let mut buf = vec![0_u8; 0x600];
        buf[0..2].copy_from_slice(b"MZ");
        buf[0x3c..0x40].copy_from_slice(&(peoff as u32).to_le_bytes());
        let mut offset = peoff;
        buf[offset..offset + 4].copy_from_slice(b"PE\0\0");
        offset += 4;
        write_u16(&mut buf[offset..offset + 2], IMAGE_FILE_MACHINE_AMD64);
        buf[offset + 2..offset + 4].copy_from_slice(&2_u16.to_le_bytes());
        buf[offset + 16..offset + 18].copy_from_slice(&0x00f0_u16.to_le_bytes());
        write_u16(
            &mut buf[offset + 18..offset + 20],
            IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL,
        );
        offset += 20;
        let optional = offset;
        buf[optional..optional + 2].copy_from_slice(&0x20b_u16.to_le_bytes());
        buf[optional + 2] = 14;
        buf[optional + 8..optional + 12].copy_from_slice(&0x400_u32.to_le_bytes());
        buf[optional + 20..optional + 24].copy_from_slice(&0x1000_u32.to_le_bytes());
        buf[optional + 24..optional + 32].copy_from_slice(&0x140000000_u64.to_le_bytes());
        buf[optional + 32..optional + 36].copy_from_slice(&0x1000_u32.to_le_bytes());
        buf[optional + 36..optional + 40].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[optional + 40..optional + 42].copy_from_slice(&6_u16.to_le_bytes());
        buf[optional + 48..optional + 50].copy_from_slice(&6_u16.to_le_bytes());
        buf[optional + 56..optional + 60].copy_from_slice(&0x3000_u32.to_le_bytes());
        buf[optional + 60..optional + 64].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[optional + 68..optional + 70].copy_from_slice(&2_u16.to_le_bytes());
        buf[optional + 70..optional + 72].copy_from_slice(&0x8160_u16.to_le_bytes());
        buf[optional + 72..optional + 80].copy_from_slice(&0x100000_u64.to_le_bytes());
        buf[optional + 80..optional + 88].copy_from_slice(&0x1000_u64.to_le_bytes());
        buf[optional + 88..optional + 96].copy_from_slice(&0x100000_u64.to_le_bytes());
        buf[optional + 96..optional + 104].copy_from_slice(&0x1000_u64.to_le_bytes());
        buf[optional + 108..optional + 112].copy_from_slice(&16_u32.to_le_bytes());
        buf[optional + 0x70..optional + 0x74].copy_from_slice(&0x1000_u32.to_le_bytes());
        buf[optional + 0x74..optional + 0x78].copy_from_slice(&0x180_u32.to_le_bytes());

        offset = optional + 0xf0;
        buf[offset..offset + 8].copy_from_slice(b".edata\0\0");
        buf[offset + 8..offset + 12].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[offset + 12..offset + 16].copy_from_slice(&0x1000_u32.to_le_bytes());
        buf[offset + 16..offset + 20].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[offset + 20..offset + 24].copy_from_slice(&(edata_raw as u32).to_le_bytes());
        buf[offset + 32..offset + 36].copy_from_slice(&0x40000040_u32.to_le_bytes());
        offset += 40;
        buf[offset..offset + 8].copy_from_slice(b".text\0\0\0");
        buf[offset + 8..offset + 12].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[offset + 12..offset + 16].copy_from_slice(&0x2000_u32.to_le_bytes());
        buf[offset + 16..offset + 20].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[offset + 20..offset + 24].copy_from_slice(&(text_raw as u32).to_le_bytes());
        buf[offset + 32..offset + 36].copy_from_slice(&0x60000020_u32.to_le_bytes());

        let export = edata_raw;
        buf[export + 12..export + 16].copy_from_slice(&0x1080_u32.to_le_bytes());
        buf[export + 16..export + 20].copy_from_slice(&ordinal_base.to_le_bytes());
        buf[export + 20..export + 24].copy_from_slice(&(targets.len() as u32).to_le_bytes());
        buf[export + 24..export + 28].copy_from_slice(&(names.len() as u32).to_le_bytes());
        buf[export + 28..export + 32].copy_from_slice(&0x1030_u32.to_le_bytes());
        buf[export + 32..export + 36].copy_from_slice(&0x1040_u32.to_le_bytes());
        buf[export + 36..export + 40].copy_from_slice(&0x1050_u32.to_le_bytes());

        let mut string_offset = 0x80usize;
        buf[export + string_offset..export + string_offset + 8].copy_from_slice(b"engine\0\0");
        string_offset += 0x10;
        for (index, target) in targets.iter().enumerate() {
            let target_rva = match target {
                SyntheticExportTarget::Zero => 0,
                SyntheticExportTarget::Direct => 0x2000,
                SyntheticExportTarget::ForwardedOrdinal(library, ordinal) => {
                    let forward = format!("{library}.#{ordinal}");
                    let bytes = forward.as_bytes();
                    buf[export + string_offset..export + string_offset + bytes.len()]
                        .copy_from_slice(bytes);
                    buf[export + string_offset + bytes.len()] = 0;
                    let rva = 0x1000 + string_offset as u32;
                    string_offset += bytes.len() + 1;
                    rva
                }
            };
            let eat = export + 0x30 + index * 4;
            buf[eat..eat + 4].copy_from_slice(&target_rva.to_le_bytes());
        }
        let mut name_string_offset = string_offset.max(0x100);
        for (name_position, (index, name)) in names.iter().enumerate() {
            let bytes = name.as_bytes();
            buf[export + name_string_offset..export + name_string_offset + bytes.len()]
                .copy_from_slice(bytes);
            buf[export + name_string_offset + bytes.len()] = 0;
            let name_rva = 0x1000 + name_string_offset as u32;
            let pointer = export + 0x40 + name_position * 4;
            buf[pointer..pointer + 4].copy_from_slice(&name_rva.to_le_bytes());
            let ordinal_pointer = export + 0x50 + name_position * 2;
            buf[ordinal_pointer..ordinal_pointer + 2]
                .copy_from_slice(&(*index as u16).to_le_bytes());
            name_string_offset += bytes.len() + 1;
        }
        buf
    }

    #[test]
    fn export_validation_tracks_eat_slots_for_aliases_and_forwarders() {
        let alias = synthetic_export_pe(
            1,
            &[
                SyntheticExportTarget::Direct,
                SyntheticExportTarget::Direct,
                SyntheticExportTarget::Zero,
            ],
            &[(0, "AliasA"), (0, "AliasB")],
        );
        let info = parse_pe_bytes(&alias).unwrap();
        assert_eq!(
            info.exported_ordinals,
            BTreeSet::from([ExportOrdinal(1), ExportOrdinal(2)])
        );
        assert_eq!(
            info.nonzero_export_slots,
            BTreeSet::from([ExportAddressIndex(0), ExportAddressIndex(1)])
        );
        assert_eq!(
            info.named_export_slots,
            BTreeSet::from([ExportAddressIndex(0)])
        );
        assert_eq!(
            info.nonzero_export_slots
                .difference(&info.named_export_slots)
                .copied()
                .collect::<Vec<_>>(),
            vec![ExportAddressIndex(1)]
        );

        let forwarded = synthetic_export_pe(
            1,
            &[SyntheticExportTarget::ForwardedOrdinal("engine", 7)],
            &[(0, "Forwarded")],
        );
        let info = parse_pe_bytes(&forwarded).unwrap();
        assert_eq!(info.exported_ordinals, BTreeSet::from([ExportOrdinal(1)]));
        assert_eq!(
            info.forwarded_exports
                .get(&ExportSymbol::Ordinal(ExportOrdinal(1))),
            Some(&ForwardedExport {
                library: "engine.dll".to_owned(),
                symbol: ExportSymbol::Ordinal(ExportOrdinal(7)),
            })
        );
    }

    #[test]
    fn export_ordinal_overflow_is_rejected_instead_of_dropped() {
        let bytes = synthetic_export_pe(0x1_0000, &[SyntheticExportTarget::Direct], &[]);
        let error = parse_pe_bytes(&bytes).unwrap_err();
        assert!(error.to_string().contains("ordinal"), "{error}");
    }

    fn graph_image(imports: &[&str], delay_imports: &[&str]) -> PeInfo {
        PeInfo {
            machine: IMAGE_FILE_MACHINE_AMD64,
            characteristics: IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL,
            exports: BTreeSet::new(),
            forwarded_exports: BTreeMap::new(),
            executable_exports: BTreeSet::new(),
            has_export_manifest: false,
            expected_exports: BTreeSet::new(),
            crt_policy: None,
            imports: imports.iter().map(|name| (*name).to_owned()).collect(),
            import_targets: BTreeMap::new(),
            delay_imports: delay_imports
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            delay_import_targets: BTreeMap::new(),
            exported_ordinals: BTreeSet::new(),
            nonzero_export_slots: BTreeSet::new(),
            named_export_slots: BTreeSet::new(),
        }
    }

    fn add_direct_export(image: &mut PeInfo, name: &str, ordinal: u16) {
        image.exports.insert(name.to_owned());
        image.exported_ordinals.insert(ExportOrdinal(ordinal));
    }

    fn add_forwarded_export(
        image: &mut PeInfo,
        name: &str,
        ordinal: u16,
        library: &str,
        target: ExportSymbol,
    ) {
        add_direct_export(image, name, ordinal);
        let forwarded = ForwardedExport {
            library: library.to_ascii_lowercase(),
            symbol: target,
        };
        let _ = image
            .forwarded_exports
            .insert(ExportSymbol::Name(name.to_owned()), forwarded.clone());
        let _ = image
            .forwarded_exports
            .insert(ExportSymbol::Ordinal(ExportOrdinal(ordinal)), forwarded);
    }

    #[test]
    fn dependency_graph_reports_the_full_missing_chain() {
        let images = BTreeMap::from([
            (
                "addin.xll".to_owned(),
                ("Addin.xll".to_owned(), graph_image(&["Engine.dll"], &[])),
            ),
            (
                "engine.dll".to_owned(),
                ("Engine.dll".to_owned(), graph_image(&[], &["Model.dll"])),
            ),
        ]);
        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Addin.xll -> Engine.dll -> model.dll")
        );
    }

    #[test]
    fn dependency_graph_rejects_missing_imported_symbol() {
        let mut addin = graph_image(&["Engine.dll"], &[]);
        addin.import_targets.insert(
            "Engine.dll".to_owned(),
            BTreeSet::from([ImportTarget::Name("SymbolV2".to_owned())]),
        );
        let mut engine = graph_image(&[], &[]);
        engine.exports.insert("SymbolV1".to_owned());
        let images = BTreeMap::from([
            ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
            ("engine.dll".to_owned(), ("Engine.dll".to_owned(), engine)),
        ]);

        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(error.to_string().contains("Engine.dll!SymbolV2"));
    }

    #[test]
    fn dependency_graph_rejects_missing_imported_ordinal() {
        let mut addin = graph_image(&[], &["Engine.dll"]);
        addin.delay_import_targets.insert(
            "Engine.dll".to_owned(),
            BTreeSet::from([ImportTarget::Ordinal(17)]),
        );
        let mut engine = graph_image(&[], &[]);
        engine.exported_ordinals.insert(ExportOrdinal(16));
        let images = BTreeMap::from([
            ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
            ("engine.dll".to_owned(), ("Engine.dll".to_owned(), engine)),
        ]);

        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(error.to_string().contains("Engine.dll!#17"));
    }

    #[test]
    fn dependency_graph_accepts_existing_name_and_ordinal_targets() {
        let mut addin = graph_image(&["Engine.dll"], &[]);
        addin.import_targets.insert(
            "Engine.dll".to_owned(),
            BTreeSet::from([
                ImportTarget::Name("Process".to_owned()),
                ImportTarget::Ordinal(17),
            ]),
        );
        let mut engine = graph_image(&[], &[]);
        engine.exports.insert("Process".to_owned());
        engine.exported_ordinals.insert(ExportOrdinal(17));
        let images = BTreeMap::from([
            ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
            ("engine.dll".to_owned(), ("Engine.dll".to_owned(), engine)),
        ]);

        validate_dependency_graph(&images, &BTreeSet::new()).unwrap();
    }

    #[test]
    fn dependency_graph_rejects_bundled_external_collision_for_regular_import() {
        let images = BTreeMap::from([
            (
                "vendor.dll".to_owned(),
                ("Vendor.dll".to_owned(), graph_image(&[], &[])),
            ),
            (
                "addin.xll".to_owned(),
                ("Addin.xll".to_owned(), graph_image(&["Vendor.dll"], &[])),
            ),
        ]);
        let external = BTreeSet::from(["vendor.dll".to_owned()]);

        let error = validate_dependency_graph(&images, &external).unwrap_err();
        assert_eq!(
            error.to_string(),
            "DLL `vendor.dll` cannot be both bundled and external"
        );
    }

    #[test]
    fn dependency_graph_rejects_bundled_external_collision_for_delay_import() {
        let images = BTreeMap::from([
            (
                "vendor.dll".to_owned(),
                ("Vendor.dll".to_owned(), graph_image(&[], &[])),
            ),
            (
                "addin.xll".to_owned(),
                ("Addin.xll".to_owned(), graph_image(&[], &["Vendor.dll"])),
            ),
        ]);
        let external = BTreeSet::from(["vendor.dll".to_owned()]);

        let error = validate_dependency_graph(&images, &external).unwrap_err();
        assert_eq!(
            error.to_string(),
            "DLL `vendor.dll` cannot be both bundled and external"
        );
    }

    #[test]
    fn dependency_graph_rejects_bundled_external_collision_for_forwarded_export() {
        let mut addin = graph_image(&[], &[]);
        add_forwarded_export(
            &mut addin,
            "Process",
            1,
            "Vendor.dll",
            ExportSymbol::Name("ProcessImpl".to_owned()),
        );
        let images = BTreeMap::from([
            ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
            (
                "vendor.dll".to_owned(),
                ("Vendor.dll".to_owned(), graph_image(&[], &[])),
            ),
        ]);
        let external = BTreeSet::from(["vendor.dll".to_owned()]);

        let error = validate_dependency_graph(&images, &external).unwrap_err();
        assert_eq!(
            error.to_string(),
            "DLL `vendor.dll` cannot be both bundled and external"
        );
    }

    #[test]
    fn dynamically_linked_msvc_runtime_is_not_treated_as_an_inbox_dll() {
        let images = BTreeMap::from([(
            "addin.xll".to_owned(),
            (
                "Addin.xll".to_owned(),
                graph_image(&["vcruntime140.dll"], &[]),
            ),
        )]);
        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(error.to_string().contains("vcruntime140.dll"));
    }

    #[test]
    fn explicit_external_import_permits_dynamic_msvc_runtime() {
        let mut bundle = StagedBundle {
            files: vec![],
            external_imports: BTreeSet::new(),
        };
        bundle
            .try_add_external_imports(["vcruntime140.dll"])
            .unwrap();
        let images = BTreeMap::from([(
            "addin.xll".to_owned(),
            (
                "Addin.xll".to_owned(),
                graph_image(&["vcruntime140.dll"], &[]),
            ),
        )]);
        assert!(validate_dependency_graph(&images, &bundle.external_imports).is_ok());
    }

    #[test]
    fn staged_bundle_external_imports_validate_all_inputs_before_mutating() {
        let mut bundle = StagedBundle {
            files: vec![],
            external_imports: BTreeSet::new(),
        };
        let error = bundle
            .try_add_external_imports(["trusted.dll", "vendor/not-a-basename.dll"])
            .unwrap_err();
        assert!(error.to_string().contains("external import"));
        assert!(bundle.external_imports.is_empty());
    }

    #[test]
    fn api_set_looking_names_are_not_implicitly_trusted() {
        let images = BTreeMap::from([(
            "addin.xll".to_owned(),
            (
                "Addin.xll".to_owned(),
                graph_image(&["api-ms-win-not-a-real-contract.dll"], &[]),
            ),
        )]);
        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("api-ms-win-not-a-real-contract.dll")
        );

        let external =
            normalize_external_imports(&["api-ms-win-not-a-real-contract.dll".to_owned()]).unwrap();
        validate_dependency_graph(&images, &external).unwrap();
    }

    #[test]
    fn explicit_external_import_is_accepted_and_validated_as_a_dll_basename() {
        let images = BTreeMap::from([(
            "addin.xll".to_owned(),
            (
                "Addin.xll".to_owned(),
                graph_image(&["Some-Inbox-Component.dll"], &[]),
            ),
        )]);
        let external =
            normalize_external_imports(&["some-inbox-component.DLL".to_owned()]).unwrap();
        validate_dependency_graph(&images, &external).unwrap();
        assert!(normalize_external_imports(&["directory/vendor.dll".to_owned()]).is_err());
        assert!(normalize_external_imports(&["directory\\vendor.dll".to_owned()]).is_err());
        assert!(normalize_external_imports(&["..\\vendor.dll".to_owned()]).is_err());
        assert!(normalize_external_imports(&["C:\\Temp\\vendor.dll".to_owned()]).is_err());
        assert!(normalize_external_imports(&["vendor.exe".to_owned()]).is_err());
    }

    #[test]
    fn public_dependency_verifier_rejects_a_bundled_system_dll_basename() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("Addin.xll");
        let bundled = directory.path().join("version.dll");
        fs::write(
            &root,
            minimal_pe(
                IMAGE_FILE_MACHINE_AMD64,
                IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL,
            ),
        )
        .unwrap();
        fs::write(
            &bundled,
            minimal_pe(
                IMAGE_FILE_MACHINE_AMD64,
                IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL,
            ),
        )
        .unwrap();

        let error = verify_pe_dependency_closure(
            &root,
            "x86_64-pc-windows-msvc",
            std::slice::from_ref(&bundled),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must not shadow Windows system DLL")
        );
    }

    #[test]
    fn dependency_graph_rejects_missing_forwarded_library() {
        let mut addin = graph_image(&[], &[]);
        add_forwarded_export(
            &mut addin,
            "Process",
            1,
            "engine.dll",
            ExportSymbol::Name("ProcessImpl".to_owned()),
        );
        let images = BTreeMap::from([("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin))]);
        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(error.to_string().contains("engine.dll"));
    }

    #[test]
    fn dependency_graph_validates_forwarded_symbol_and_chain() {
        let mut addin = graph_image(&[], &[]);
        add_forwarded_export(
            &mut addin,
            "Process",
            1,
            "engine.dll",
            ExportSymbol::Name("ProcessImpl".to_owned()),
        );
        let mut engine = graph_image(&[], &[]);
        add_forwarded_export(
            &mut engine,
            "ProcessImpl",
            2,
            "model.dll",
            ExportSymbol::Ordinal(ExportOrdinal(7)),
        );
        let mut model = graph_image(&[], &[]);
        model.exported_ordinals.insert(ExportOrdinal(7));
        let images = BTreeMap::from([
            ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
            ("engine.dll".to_owned(), ("Engine.dll".to_owned(), engine)),
            ("model.dll".to_owned(), ("Model.dll".to_owned(), model)),
        ]);
        validate_dependency_graph(&images, &BTreeSet::new()).unwrap();
    }

    #[test]
    fn dependency_graph_rejects_missing_forwarded_symbol() {
        let mut addin = graph_image(&[], &[]);
        add_forwarded_export(
            &mut addin,
            "Process",
            1,
            "engine.dll",
            ExportSymbol::Name("Missing".to_owned()),
        );
        let images = BTreeMap::from([
            ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
            (
                "engine.dll".to_owned(),
                ("Engine.dll".to_owned(), graph_image(&[], &[])),
            ),
        ]);
        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("forwarded export target is missing")
        );
    }

    #[test]
    fn dependency_graph_rejects_forwarder_cycles() {
        let mut addin = graph_image(&[], &[]);
        add_forwarded_export(
            &mut addin,
            "Process",
            1,
            "engine.dll",
            ExportSymbol::Name("ProcessImpl".to_owned()),
        );
        let mut engine = graph_image(&[], &[]);
        add_forwarded_export(
            &mut engine,
            "ProcessImpl",
            2,
            "addin.xll",
            ExportSymbol::Name("Process".to_owned()),
        );
        let images = BTreeMap::from([
            ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
            ("engine.dll".to_owned(), ("Engine.dll".to_owned(), engine)),
        ]);
        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(error.to_string().contains("cyclic forwarded export"));
    }

    #[cfg(unix)]
    #[test]
    fn bundle_rejects_symlink_escape_and_strict_mode_rejects_any_symlink() {
        use std::os::unix::fs::symlink;

        let manifest = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("Vendor.dll"), b"outside").unwrap();
        symlink(
            outside.path().join("Vendor.dll"),
            manifest.path().join("Escape.dll"),
        )
        .unwrap();
        let strict_error = resolve_bundle_files(
            manifest.path(),
            "x86_64-pc-windows-msvc",
            &["Escape.dll".to_owned()],
        )
        .unwrap_err();
        assert!(strict_error.to_string().contains("rejects symlink"));

        let relaxed_escape_error = resolve_bundle_files_with_policy(
            manifest.path(),
            "x86_64-pc-windows-msvc",
            &["Escape.dll".to_owned()],
            &[],
            false,
        )
        .unwrap_err();
        assert!(
            relaxed_escape_error
                .to_string()
                .contains("escapes manifest directory")
        );

        fs::write(manifest.path().join("Inside.dll"), b"inside").unwrap();
        symlink(
            manifest.path().join("Inside.dll"),
            manifest.path().join("Alias.dll"),
        )
        .unwrap();
        let relaxed = resolve_bundle_files_with_policy(
            manifest.path(),
            "x86_64-pc-windows-msvc",
            &["Alias.dll".to_owned()],
            &[],
            false,
        )
        .unwrap();
        assert_eq!(relaxed.resolved_files().count(), 1);
        let strict = resolve_bundle_files_with_policy(
            manifest.path(),
            "x86_64-pc-windows-msvc",
            &["Alias.dll".to_owned()],
            &[],
            true,
        )
        .unwrap_err();
        assert!(strict.to_string().contains("rejects symlink"));
    }

    #[test]
    fn private_staging_directory_rejects_existing_destination() {
        let destination = tempfile::tempdir().unwrap();
        let error = PrivateStagingDirectory::create(destination.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("private staging directory already exists")
        );
    }

    #[cfg(any(unix, target_os = "windows"))]
    #[test]
    fn file_identity_distinguishes_replaced_sources() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.dll");
        let second = directory.path().join("second.dll");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();

        let opened = std::fs::File::open(&first).unwrap();
        let same = std::fs::File::open(&first).unwrap();
        let different = std::fs::File::open(&second).unwrap();
        assert!(same_file_identity(&opened, &same).unwrap());
        assert!(!same_file_identity(&opened, &different).unwrap());
    }

    #[cfg(target_os = "windows")]
    struct BlockingAfterOpenObserver {
        entered: std::sync::Arc<std::sync::Barrier>,
        release: std::sync::Arc<std::sync::Barrier>,
    }

    #[cfg(target_os = "windows")]
    impl SnapshotObserver for BlockingAfterOpenObserver {
        fn after_open(&self, _path: &Path) {
            self.entered.wait();
            self.release.wait();
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn snapshot_denies_in_place_writers() {
        use crate::win32::ERROR_SHARING_VIOLATION;

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("Engine.dll");
        fs::write(&source, vec![0x41; 1024 * 1024]).unwrap();

        let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release = std::sync::Arc::new(std::sync::Barrier::new(2));
        let observer = BlockingAfterOpenObserver {
            entered: std::sync::Arc::clone(&entered),
            release: std::sync::Arc::clone(&release),
        };

        let manifest_directory = directory.path().to_owned();
        let resolver = std::thread::spawn(move || {
            resolve_bundle_files_with_policy_impl(
                &manifest_directory,
                "x86_64-pc-windows-msvc",
                &["Engine.dll".to_owned()],
                &[],
                false,
                &observer,
            )
        });

        entered.wait();

        let error = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));

        release.wait();
        resolver.join().unwrap().unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn snapshot_rejects_source_already_open_for_writing() {
        use crate::win32::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        use std::os::windows::fs::OpenOptionsExt;

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("Engine.dll");
        fs::write(&source, b"original").unwrap();

        let _writer = std::fs::OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&source)
            .unwrap();

        let error = resolve_bundle_files(
            directory.path(),
            "x86_64-pc-windows-msvc",
            &["Engine.dll".to_owned()],
        )
        .unwrap_err();

        assert!(matches!(error, PackageError::BundleSourceBusy { .. }));
    }

    #[cfg(unix)]
    struct BlockingAfterFirstChunkObserver {
        entered: std::sync::Arc<std::sync::Barrier>,
        release: std::sync::Arc<std::sync::Barrier>,
    }

    #[cfg(unix)]
    impl SnapshotObserver for BlockingAfterFirstChunkObserver {
        fn after_first_chunk(&self, _path: &Path) {
            self.entered.wait();
            self.release.wait();
        }
    }

    #[cfg(unix)]
    #[test]
    fn same_length_in_place_mutation_during_snapshot_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("Engine.dll");
        let initial = vec![0x11; 4 * 1024 * 1024];
        let replacement = vec![0x22; initial.len()];
        fs::write(&source, &initial).unwrap();

        let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release = std::sync::Arc::new(std::sync::Barrier::new(2));
        let observer = BlockingAfterFirstChunkObserver {
            entered: std::sync::Arc::clone(&entered),
            release: std::sync::Arc::clone(&release),
        };

        let manifest_directory = directory.path().to_owned();
        let resolver = std::thread::spawn(move || {
            resolve_bundle_files_with_policy_impl(
                &manifest_directory,
                "x86_64-pc-windows-msvc",
                &["Engine.dll".to_owned()],
                &[],
                false,
                &observer,
            )
        });

        entered.wait();
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap();
        writer.write_all(&replacement).unwrap();
        writer.flush().unwrap();
        drop(writer);
        release.wait();

        let error = resolver.join().unwrap().unwrap_err();
        assert!(matches!(error, PackageError::UnstableBundleSource { .. }));
    }

    #[test]
    fn snapshot_file_keeps_the_bytes_from_its_open_handle() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("Addin.dll");
        fs::write(&source, b"original bytes").unwrap();

        let snapshot = snapshot_file("x86_64-pc-windows-msvc", &source).unwrap();
        fs::write(&source, b"replacement bytes").unwrap();

        assert_eq!(snapshot.as_ref(), b"original bytes");
    }

    #[test]
    fn verified_artifacts_keep_bytes_and_identity_for_commit_checks() {
        let directory = tempfile::tempdir().unwrap();
        let staged = directory.path().join("Engine.dll");
        fs::write(&staged, b"stable bytes").unwrap();
        let bytes: Arc<[u8]> = Arc::from(&b"stable bytes"[..]);
        let artifact = verified_artifact(
            PathBuf::from("Engine.dll"),
            bytes,
            fs::metadata(&staged).unwrap().permissions(),
        );
        let manifest = serde_json::to_vec(&serde_json::json!({
            "files": [{
                "relative_path": "Engine.dll",
                "size": artifact.size(),
                "sha256": artifact.sha256_hex(),
            }]
        }))
        .unwrap();
        let package = VerifiedPackage {
            artifacts: vec![artifact],
            expected_names: BTreeSet::from(["engine.dll".to_owned()]),
        }
        .with_manifest_bytes(manifest.clone())
        .unwrap();
        fs::write(directory.path().join("build-manifest.json"), &manifest).unwrap();

        let artifact = &package.artifacts()[0];
        assert_eq!(artifact.relative_path(), Path::new("Engine.dll"));
        assert_eq!(artifact.bytes(), b"stable bytes");
        assert_eq!(artifact.size(), 12);
        assert_eq!(
            artifact.sha256_hex(),
            "3821461753e58afa7abe81ccec8ea5ac178ea27ee92ede53771a95a101928e40"
        );
        let prepared = package
            .prepare_commit(directory.path(), "x86_64-pc-windows-msvc")
            .unwrap();
        prepared.verify_source_contents().unwrap();

        let rebuilt_parent = tempfile::tempdir().unwrap();
        let rebuilt = rebuilt_parent.path().join("rebuilt");
        let rebuilt_directory = PrivateStagingDirectory::create(&rebuilt).unwrap();
        package.materialize(&rebuilt_directory).unwrap();
        assert_eq!(
            fs::read(rebuilt.join("Engine.dll")).unwrap(),
            b"stable bytes"
        );

        fs::write(&staged, b"changed bytes").unwrap();
        assert!(
            package
                .prepare_commit(directory.path(), "x86_64-pc-windows-msvc")
                .is_err()
        );
    }

    #[test]
    fn prepared_package_rejects_unknown_entries_and_manifest_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let staged = directory.path().join("Engine.dll");
        fs::write(&staged, b"stable bytes").unwrap();
        let artifact = verified_artifact(
            PathBuf::from("Engine.dll"),
            Arc::from(&b"stable bytes"[..]),
            fs::metadata(&staged).unwrap().permissions(),
        );
        let manifest = serde_json::to_vec(&serde_json::json!({
            "files": [{
                "relative_path": "Engine.dll",
                "size": artifact.size(),
                "sha256": artifact.sha256_hex(),
            }]
        }))
        .unwrap();
        let package = VerifiedPackage {
            artifacts: vec![artifact],
            expected_names: BTreeSet::from(["engine.dll".to_owned()]),
        }
        .with_manifest_bytes(manifest.clone())
        .unwrap();
        fs::write(directory.path().join("build-manifest.json"), manifest).unwrap();
        let prepared = package
            .prepare_commit(directory.path(), "x86_64-pc-windows-msvc")
            .unwrap();

        fs::write(directory.path().join("version.dll"), b"shadow").unwrap();
        assert!(prepared.verify_source_contents().is_err());
        fs::remove_file(directory.path().join("version.dll")).unwrap();

        fs::write(directory.path().join("build-manifest.json"), b"{}").unwrap();
        assert!(prepared.verify_source_contents().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn prepared_package_opens_entries_without_following_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let staged = directory.path().join("Engine.dll");
        let replacement = directory.path().join("replacement.dll");
        fs::write(&staged, b"stable bytes").unwrap();
        let artifact = verified_artifact(
            PathBuf::from("Engine.dll"),
            Arc::from(&b"stable bytes"[..]),
            fs::metadata(&staged).unwrap().permissions(),
        );
        let manifest = serde_json::to_vec(&serde_json::json!({
            "files": [{
                "relative_path": "Engine.dll",
                "size": artifact.size(),
                "sha256": artifact.sha256_hex(),
            }]
        }))
        .unwrap();
        let package = VerifiedPackage {
            artifacts: vec![artifact],
            expected_names: BTreeSet::from(["engine.dll".to_owned()]),
        }
        .with_manifest_bytes(manifest.clone())
        .unwrap();
        fs::write(directory.path().join("build-manifest.json"), manifest).unwrap();
        let prepared = package
            .prepare_commit(directory.path(), "x86_64-pc-windows-msvc")
            .unwrap();

        fs::write(&replacement, b"stable bytes").unwrap();
        fs::remove_file(&staged).unwrap();
        symlink(&replacement, &staged).unwrap();
        assert!(prepared.verify_source_contents().is_err());
    }

    #[test]
    fn prepared_package_rejects_replaced_staging_directory() {
        let parent = tempfile::tempdir().unwrap();
        let staging = parent.path().join("staging");
        fs::create_dir(&staging).unwrap();
        let staged = staging.join("Engine.dll");
        fs::write(&staged, b"stable bytes").unwrap();
        let artifact = verified_artifact(
            PathBuf::from("Engine.dll"),
            Arc::from(&b"stable bytes"[..]),
            fs::metadata(&staged).unwrap().permissions(),
        );
        let manifest = serde_json::to_vec(&serde_json::json!({
            "files": [{
                "relative_path": "Engine.dll",
                "size": artifact.size(),
                "sha256": artifact.sha256_hex(),
            }]
        }))
        .unwrap();
        let package = VerifiedPackage {
            artifacts: vec![artifact],
            expected_names: BTreeSet::from(["engine.dll".to_owned()]),
        }
        .with_manifest_bytes(manifest.clone())
        .unwrap();
        fs::write(staging.join("build-manifest.json"), manifest).unwrap();
        let prepared = package
            .prepare_commit(&staging, "x86_64-pc-windows-msvc")
            .unwrap();

        let moved = parent.path().join("moved-staging");
        fs::rename(&staging, &moved).unwrap();
        let replacement = PrivateStagingDirectory::create(&staging).unwrap();
        package.materialize(&replacement).unwrap();

        assert!(prepared.verify_source_contents().is_err());
    }

    #[test]
    fn prepared_directory_rejects_nested_file_mutation() {
        let parent = tempfile::tempdir().unwrap();
        let staging = parent.path().join("staging");
        let package = staging.join("package-a");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("manifest.json"), b"original manifest").unwrap();
        fs::write(package.join("addin.xll"), b"original addin").unwrap();

        let prepared = PreparedDirectoryCommit::prepare(&staging, &["package-a"]).unwrap();
        fs::write(package.join("addin.xll"), b"changed addin").unwrap();

        assert!(prepared.verify_source_contents().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn private_staging_verify_rejects_replaced_path() {
        let parent = tempfile::tempdir().unwrap();
        let staging = parent.path().join("staging");
        let capability = PrivateStagingDirectory::create(&staging).unwrap();
        let moved = parent.path().join("moved-staging");
        fs::rename(&staging, &moved).unwrap();
        let replacement = PrivateStagingDirectory::create(&staging).unwrap();

        assert!(matches!(
            capability.verify(),
            Err(PackageError::StagingDirectoryReplaced { .. })
        ));

        drop(replacement);
        drop(capability);
    }

    #[test]
    fn stage_bundle_uses_the_resolved_file_snapshot() {
        let source = tempfile::tempdir().unwrap();
        let source_path = source.path().join("Engine.dll");
        fs::write(&source_path, b"resolved bytes").unwrap();
        let bundle = resolve_bundle_files(
            source.path(),
            "x86_64-pc-windows-msvc",
            &["Engine.dll".to_owned()],
        )
        .unwrap();

        fs::write(&source_path, b"replacement bytes").unwrap();
        let destination = source.path().join("staging");
        let staging_directory = PrivateStagingDirectory::create(&destination).unwrap();
        stage_bundle(&bundle, &staging_directory).unwrap();

        assert_eq!(
            fs::read(destination.join("Engine.dll")).unwrap(),
            b"resolved bytes"
        );
    }

    #[test]
    fn bundle_rejects_windows_system_dll_name_collisions() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("version.dll"), b"not the system DLL").unwrap();

        let error = resolve_bundle_files(
            source.path(),
            "x86_64-pc-windows-msvc",
            &["version.dll".to_owned()],
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("must not shadow Windows system DLL")
        );
    }

    #[test]
    fn va_based_delay_load_import_descriptor_is_rejected() {
        let peoff = 0x80usize;
        let raw = 0x200usize;
        let section_rva = 0x1000u32;
        let mut buf = vec![0u8; 0x400];
        buf[0..2].copy_from_slice(b"MZ");
        buf[0x3c..0x40].copy_from_slice(&(peoff as u32).to_le_bytes());
        let mut o = peoff;
        buf[o..o + 4].copy_from_slice(b"PE\0\0");
        o += 4;
        buf[o..o + 2].copy_from_slice(&0x8664u16.to_le_bytes());
        buf[o + 2..o + 4].copy_from_slice(&1u16.to_le_bytes());
        buf[o + 16..o + 18].copy_from_slice(&0x00f0u16.to_le_bytes());
        buf[o + 18..o + 20].copy_from_slice(&0x2022u16.to_le_bytes());
        o += 20;
        let opt = o;
        buf[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        buf[opt + 2] = 14;
        buf[opt + 8..opt + 12].copy_from_slice(&0x200u32.to_le_bytes());
        buf[opt + 20..opt + 24].copy_from_slice(&0x1000u32.to_le_bytes());
        buf[opt + 24..opt + 32].copy_from_slice(&0x10000u64.to_le_bytes());
        buf[opt + 32..opt + 36].copy_from_slice(&0x1000u32.to_le_bytes());
        buf[opt + 36..opt + 40].copy_from_slice(&0x200u32.to_le_bytes());
        buf[opt + 40..opt + 42].copy_from_slice(&6u16.to_le_bytes());
        buf[opt + 48..opt + 50].copy_from_slice(&6u16.to_le_bytes());
        buf[opt + 56..opt + 60].copy_from_slice(&0x2000u32.to_le_bytes());
        buf[opt + 60..opt + 64].copy_from_slice(&0x200u32.to_le_bytes());
        buf[opt + 68..opt + 70].copy_from_slice(&2u16.to_le_bytes());
        buf[opt + 70..opt + 72].copy_from_slice(&0x8160u16.to_le_bytes());
        buf[opt + 72..opt + 80].copy_from_slice(&0x100000u64.to_le_bytes());
        buf[opt + 80..opt + 88].copy_from_slice(&0x1000u64.to_le_bytes());
        buf[opt + 88..opt + 96].copy_from_slice(&0x100000u64.to_le_bytes());
        buf[opt + 96..opt + 104].copy_from_slice(&0x1000u64.to_le_bytes());
        buf[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
        buf[opt + 0x70 + 13 * 8..opt + 0x70 + 13 * 8 + 4]
            .copy_from_slice(&section_rva.to_le_bytes());
        buf[opt + 0x70 + 13 * 8 + 4..opt + 0x70 + 13 * 8 + 8]
            .copy_from_slice(&0x40u32.to_le_bytes());

        o = opt + 0xf0;
        buf[o..o + 8].copy_from_slice(b".rdata\0\0");
        o += 8;
        buf[o..o + 4].copy_from_slice(&0x200u32.to_le_bytes());
        buf[o + 4..o + 8].copy_from_slice(&section_rva.to_le_bytes());
        buf[o + 8..o + 12].copy_from_slice(&0x200u32.to_le_bytes());
        buf[o + 12..o + 16].copy_from_slice(&(raw as u32).to_le_bytes());
        buf[o + 32..o + 36].copy_from_slice(&0x40000040u32.to_le_bytes());

        buf[raw..raw + 4].copy_from_slice(&0u32.to_le_bytes());
        buf[raw + 4..raw + 8].copy_from_slice(&(section_rva + 0x40).to_le_bytes());
        let name = b"evil.dll\0";
        buf[raw + 0x40..raw + 0x40 + name.len()].copy_from_slice(name);

        let error = parse_pe_bytes(&buf).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported VA-based delay load import descriptor")
        );
    }

    proptest! {
        #[test]
        fn generated_bytes_never_panic_the_pe_parser(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
            prop_assert!(std::panic::catch_unwind(|| parse_pe_bytes(&bytes)).is_ok());
        }
    }

    #[test]
    fn verify_xll_exports_rejects_unmanifested_custom_entry_and_ordinals() {
        let temp_dir = tempfile::tempdir().unwrap();
        let xll_path = temp_dir.path().join("test.xll");

        let mut info = PeInfo {
            exports: BTreeSet::from(["xlAutoOpen".to_string(), "CustomEntry".to_string()]),
            executable_exports: BTreeSet::from([
                "xlAutoOpen".to_string(),
                "CustomEntry".to_string(),
            ]),
            expected_exports: BTreeSet::from(["xlAutoOpen".to_string()]),
            ..Default::default()
        };

        assert!(verify_xll_exports(&info, &xll_path, &[]).is_err());

        info.exports.remove("CustomEntry");
        info.executable_exports.remove("CustomEntry");
        info.nonzero_export_slots
            .extend([ExportAddressIndex(1), ExportAddressIndex(2)]);
        assert!(verify_xll_exports(&info, &xll_path, &[]).is_err());
    }
}
