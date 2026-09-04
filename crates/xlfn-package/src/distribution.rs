#![allow(
    unsafe_code,
    reason = "Distribution durability uses intentional OS file-system boundaries"
)]

//! Durable state vocabulary for distribution commits.
//!
//! The package layer owns the complete transaction, including lock ownership,
//! journal durability, stale recovery, rollback, and quarantine. Callers only
//! provide prepared evidence; they do not sequence destructive file-system
//! transitions or supply verification closures.

use crate::{DirectoryIdentity, PackageResult, PrivateStagingDirectory};
use fs_err as fs;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// File-system operations used by the distribution transaction.
///
/// The trait is deliberately small so the transaction engine can be tested
/// with failure injection without moving file-system policy back into the CLI.
pub trait DistributionFileOps {
    /// Renames a file-system entry without copying it.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Removes a directory tree.
    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;

    /// Persists directory-entry changes as far as the host platform permits.
    fn sync_directory(&self, path: &Path) -> io::Result<()>;
}

/// Validates a distribution destination before any replacement is attempted.
pub fn validate_output_destination(destination: &Path) -> PackageResult {
    crate::validate_directory_path(destination)?;
    Ok(())
}

/// Production implementation of [`DistributionFileOps`].
pub struct SystemDistributionFileOps;

impl DistributionFileOps for SystemDistributionFileOps {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        rename_path(from, to)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir_all(path)
    }

    fn sync_directory(&self, path: &Path) -> io::Result<()> {
        sync_directory(path)
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LockFileIdentity {
    dev: u64,
    ino: u64,
}

/// Exclusive lock for one destination distribution.
///
/// The lock owns the open file handle and validates that the pathname still
/// refers to the same inode before every destructive phase.
pub struct DistributionCommitGuard {
    lock_file: std::fs::File,
    lock_path: PathBuf,
    #[cfg(unix)]
    lock_identity: LockFileIdentity,
    destination: PathBuf,
}

impl DistributionCommitGuard {
    /// Acquires the destination lock below `parent`.
    pub fn acquire(parent: &Path, destination_name: &str) -> PackageResult<Self> {
        let lock_path = parent.join(format!(".{destination_name}.lock"));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;

            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }

        #[cfg(target_os = "windows")]
        {
            use crate::win32::FILE_FLAG_OPEN_REPARSE_POINT;
            use std::os::windows::fs::OpenOptionsExt;

            options
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT as u32)
                .share_mode(0);
        }

        let lock_file = options.open(&lock_path).map_err(|error| {
            format!(
                "failed to open distribution commit lock {}: {error}",
                lock_path.display()
            )
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;

            // SAFETY: the file descriptor is valid for the lifetime of the
            // lock file; LOCK_EX serializes operations that use this inode,
            // while ensure_held detects pathname replacement.
            let status = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
            if status != 0 {
                return Err(format!(
                    "failed to lock distribution commit {}: {}",
                    lock_path.display(),
                    io::Error::last_os_error()
                )
                .into());
            }
        }

        #[cfg(unix)]
        let lock_identity = lock_file_identity(&lock_file)?;
        let guard = Self {
            lock_file,
            lock_path,
            #[cfg(unix)]
            lock_identity,
            destination: parent.join(destination_name),
        };
        guard.ensure_held()?;
        Ok(guard)
    }

    /// Verifies that the held lock still refers to its original pathname.
    pub fn ensure_held(&self) -> io::Result<()> {
        let metadata = self.lock_file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::other(
                "distribution commit lock handle is not a file",
            ));
        }

        #[cfg(unix)]
        {
            let path_metadata = fs::symlink_metadata(&self.lock_path)?;
            if !path_metadata.is_file() {
                return Err(io::Error::other(
                    "distribution commit lock path is not a regular file",
                ));
            }
            let path_identity = lock_file_identity_from_metadata(&path_metadata);
            if path_identity != self.lock_identity {
                return Err(io::Error::other(
                    "distribution commit lock path was replaced while held",
                ));
            }
        }

        #[cfg(target_os = "windows")]
        {
            // share_mode(0) on the held handle prevents pathname deletion or
            // replacement while the lock is held; the path check below also
            // detects an externally forced replacement before a destructive
            // phase.
            let path_metadata = fs::symlink_metadata(&self.lock_path)?;
            if !path_metadata.is_file() {
                return Err(io::Error::other(
                    "distribution commit lock path is not a regular file",
                ));
            }
        }

        Ok(())
    }

    /// Returns the destination protected by this lock.
    #[must_use]
    pub fn destination(&self) -> &Path {
        &self.destination
    }
}

#[cfg(unix)]
fn lock_file_identity(file: &std::fs::File) -> io::Result<LockFileIdentity> {
    Ok(lock_file_identity_from_metadata(&file.metadata()?))
}

#[cfg(unix)]
fn lock_file_identity_from_metadata(metadata: &std::fs::Metadata) -> LockFileIdentity {
    use std::os::unix::fs::MetadataExt;

    LockFileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

/// Renames a path using the platform's durable, bounded-retry operation.
pub fn rename_path(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        // std::fs::rename can use the newer Windows rename-by-handle path when
        // MoveFileExW alone is rejected, while preserving same-volume rename
        // semantics. The bounded retry covers transient scanner locks.
        retry_windows_path_operation(|| fs::rename(from, to))
    }

    #[cfg(not(target_os = "windows"))]
    fs::rename(from, to)
}

/// Atomically replaces a file and requests write-through durability on Windows.
pub fn atomic_replace_file(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use crate::win32::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};

        move_file_ex_with_retry(
            from,
            to,
            (MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) as u32,
        )
    }

    #[cfg(not(target_os = "windows"))]
    fs::rename(from, to)
}

/// Flushes a directory entry where the host platform supports it.
pub fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let directory = std::fs::File::open(path)?;
        directory.sync_all()
    }
    #[cfg(target_os = "windows")]
    {
        use crate::win32::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        use std::os::windows::fs::OpenOptionsExt;

        // Windows does not expose a portable parent-directory fsync. File
        // contents are flushed before this call, and every publishing rename
        // uses MoveFileExW with MOVEFILE_WRITE_THROUGH. Reopen the directory
        // to validate that it still resolves to a directory, but do not call
        // File::sync_all: that maps to FlushFileBuffers on a read-only handle
        // and deterministically returns ERROR_ACCESS_DENIED.
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT) as u32)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE) as u32);
        let directory = options.open(path)?;
        if !directory.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!(
                    "directory synchronization target is not a directory: {}",
                    path.display()
                ),
            ));
        }
        Ok(())
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory synchronization is unsupported on this platform",
        ))
    }
}

/// Syncs both directory entries affected by a rename.
pub fn sync_rename_parents(
    from: &Path,
    to: &Path,
    file_ops: &impl DistributionFileOps,
) -> io::Result<()> {
    let from_parent = from
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let to_parent = to
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    file_ops.sync_directory(from_parent)?;
    if to_parent != from_parent {
        file_ops.sync_directory(to_parent)?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn retryable_windows_path_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
    ) || matches!(error.raw_os_error(), Some(5) | Some(32) | Some(33))
}

#[cfg(target_os = "windows")]
fn retry_windows_path_operation(mut operation: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    const ATTEMPTS: usize = 24;
    let mut delay = std::time::Duration::from_millis(10);
    for attempt in 0..ATTEMPTS {
        match operation() {
            Ok(()) => return Ok(()),
            Err(error) if attempt + 1 < ATTEMPTS && retryable_windows_path_error(&error) => {
                std::thread::sleep(delay);
                delay = delay
                    .saturating_mul(2)
                    .min(std::time::Duration::from_millis(500));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded path-operation loop always returns")
}

#[cfg(target_os = "windows")]
fn move_file_ex_with_retry(from: &Path, to: &Path, flags: u32) -> io::Result<()> {
    use crate::win32::MoveFileExW;
    use std::os::windows::ffi::OsStrExt;

    let from_wide = from
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let to_wide = to
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    retry_windows_path_operation(|| {
        // SAFETY: both paths are live, NUL-terminated buffers for this call.
        if unsafe { MoveFileExW(from_wide.as_ptr(), to_wide.as_ptr(), flags) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

pub const TRANSACTION_JOURNAL: &str = "journal";
pub const TRANSACTION_SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransactionState {
    Prepared,
    NoPrevious,
    PreviousSaved,
    InstallPending,
    InstallPendingNoPrevious,
    Installed,
    InstalledNoPrevious,
    RollbackPending,
    RollbackPendingNoPrevious,
    RolledBack,
    // The installed directory is authoritative. `previous`, when present,
    // is cleanup payload only and is never a recovery source in this state.
    Committed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionJournal {
    pub schema: u32,
    pub transaction_id: String,
    pub destination_name: String,
    pub parent_identity: DirectoryIdentity,
    pub transaction_identity: DirectoryIdentity,
    pub destination_identity: Option<DirectoryIdentity>,
    pub state: TransactionState,
    pub previous_identity: Option<DirectoryIdentity>,
    pub installed_identity: Option<DirectoryIdentity>,
    pub sequence: u64,
    pub checksum: String,
}

impl TransactionJournal {
    pub fn new(
        transaction_id: String,
        destination_name: String,
        parent_identity: DirectoryIdentity,
        transaction_identity: DirectoryIdentity,
        destination_identity: Option<DirectoryIdentity>,
    ) -> PackageResult<Self> {
        let mut journal = Self {
            schema: TRANSACTION_SCHEMA,
            transaction_id,
            destination_name,
            parent_identity,
            transaction_identity,
            destination_identity,
            state: TransactionState::Prepared,
            previous_identity: None,
            installed_identity: None,
            sequence: 0,
            checksum: String::new(),
        };
        journal.refresh_checksum()?;
        Ok(journal)
    }

    pub fn refresh_checksum(&mut self) -> PackageResult {
        self.checksum = journal_checksum(self)?;
        Ok(())
    }
}

/// Reads and validates a durable transaction journal.
pub fn read_transaction_journal(path: &Path) -> PackageResult<TransactionJournal> {
    let bytes = fs::read(path)?;
    let journal: TransactionJournal = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "distribution transaction journal is invalid: {}: {error}",
            path.display()
        )
    })?;
    if journal.schema != TRANSACTION_SCHEMA {
        return Err(format!(
            "unsupported distribution transaction journal schema {} in {}",
            journal.schema,
            path.display()
        )
        .into());
    }
    if journal.sequence == 0 {
        return Err(format!(
            "distribution transaction journal has no committed sequence: {}",
            path.display()
        )
        .into());
    }
    if journal_checksum(&journal)? != journal.checksum {
        return Err(format!(
            "distribution transaction journal checksum mismatch: {}",
            path.display()
        )
        .into());
    }
    Ok(journal)
}

/// Writes the next journal state with the transaction's durability protocol.
pub fn write_transaction_state(
    journal: &Path,
    parent: &Path,
    transaction: &mut TransactionJournal,
    state: TransactionState,
    file_ops: &impl DistributionFileOps,
) -> PackageResult {
    // Write the next state, sync the file, atomically replace the journal, then
    // sync both directory entries that make the replacement observable after a
    // power loss.
    transaction.state = state;
    transaction.sequence = transaction
        .sequence
        .checked_add(1)
        .ok_or_else(|| "distribution transaction journal sequence overflow".to_owned())?;
    transaction.refresh_checksum()?;
    let encoded = serde_json::to_vec_pretty(transaction)?;
    let next = journal.with_file_name(format!("{}.next", TRANSACTION_JOURNAL));
    let mut next_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&next)?;
    next_file.write_all(&encoded)?;
    next_file.sync_all()?;
    drop(next_file);
    atomic_replace_file(&next, journal)?;
    let transaction_directory = journal
        .parent()
        .ok_or_else(|| "transaction journal has no parent directory".to_owned())?;
    file_ops.sync_directory(transaction_directory)?;
    file_ops.sync_directory(parent)?;
    Ok(())
}

/// Returns the identity of an existing directory, or `None` when it is absent.
pub fn optional_directory_identity(path: &Path) -> PackageResult<Option<DirectoryIdentity>> {
    match fs::symlink_metadata(path) {
        Ok(_) => crate::directory_identity(path).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Requires a directory to retain a previously recorded identity.
pub fn require_directory_identity(
    path: &Path,
    expected: DirectoryIdentity,
    label: &str,
) -> PackageResult {
    let actual = crate::directory_identity(path)?;
    if actual != expected {
        return Err(format!("{label} identity changed: {}", path.display()).into());
    }
    Ok(())
}

/// Lists transaction payloads while excluding journal control files.
pub fn transaction_payloads(transaction: &Path) -> PackageResult<Vec<PathBuf>> {
    Ok(fs::read_dir(transaction)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| {
            !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(TRANSACTION_JOURNAL) | Some("journal.next")
            )
        })
        .collect())
}

/// Extracts a transaction identifier from its canonical directory name.
pub fn transaction_id(transaction: &Path, prefix: &str) -> PackageResult<String> {
    let name = transaction
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "transaction directory name is not valid UTF-8".to_owned())?;
    let suffix = name
        .strip_prefix(prefix)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "transaction directory has an invalid name".to_owned())?;
    Ok(suffix.strip_prefix("private-").unwrap_or(suffix).to_owned())
}

/// Reports whether a transaction is still in its private pre-publication form.
pub fn is_private_transaction(transaction: &Path, prefix: &str) -> PackageResult<bool> {
    let name = transaction
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "transaction directory name is not valid UTF-8".to_owned())?;
    let suffix = name
        .strip_prefix(prefix)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "transaction directory has an invalid name".to_owned())?;
    Ok(suffix.starts_with("private-"))
}

/// Validates the journal's identity and destination provenance.
pub fn validate_transaction_provenance(
    parent: &Path,
    destination_name: &str,
    prefix: &str,
    transaction: &Path,
    journal: &TransactionJournal,
) -> PackageResult<PrivateStagingDirectory> {
    let transaction_directory = PrivateStagingDirectory::open(transaction)?;
    transaction_directory.verify()?;
    let expected_id = transaction_id(transaction, prefix)?;
    if journal.transaction_id != expected_id {
        return Err(format!(
            "distribution transaction ID does not match its directory: {}",
            transaction.display()
        )
        .into());
    }
    if journal.destination_name != destination_name {
        return Err(format!(
            "distribution transaction destination does not match its directory: {}",
            transaction.display()
        )
        .into());
    }
    require_directory_identity(parent, journal.parent_identity, "transaction parent")?;
    require_directory_identity(
        transaction,
        journal.transaction_identity,
        "transaction directory",
    )?;
    Ok(transaction_directory)
}

/// Requires a destination path to be absent.
pub fn ensure_destination_absent(destination: &Path) -> PackageResult {
    match fs::symlink_metadata(destination) {
        Ok(_) => Err(format!("destination unexpectedly exists: {}", destination.display()).into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Removes an empty transaction directory after validating its private identity.
pub fn remove_empty_transaction(
    parent: &Path,
    transaction: &Path,
    file_ops: &impl DistributionFileOps,
) -> PackageResult {
    let transaction_directory = PrivateStagingDirectory::open(transaction)?;
    transaction_directory.verify()?;
    file_ops.remove_dir_all(transaction)?;
    file_ops.sync_directory(parent)?;
    Ok(())
}

pub fn journal_checksum(journal: &TransactionJournal) -> PackageResult<String> {
    let mut unsigned = journal.clone();
    unsigned.checksum.clear();
    let encoded = serde_json::to_vec(&unsigned)?;
    let digest = Sha256::digest(encoded);
    let mut checksum = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut checksum, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(checksum)
}

use crate::directory_identity;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Errors exposed by the package-owned distribution facade.
#[derive(Debug, thiserror::Error)]
pub enum DistributionError {
    #[error(transparent)]
    Recovery(#[from] DistributionRecoveryError),
    #[error(transparent)]
    Quarantined(#[from] DistributionRecoveryQuarantineError),
    #[error(transparent)]
    Package(#[from] crate::PackageError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid distribution transaction: {0}")]
    Invalid(String),
    #[error("{phase}: {primary}; recording state failed: {state}")]
    StateRecordingFailure {
        phase: &'static str,
        primary: Box<DistributionError>,
        #[source]
        state: Box<DistributionError>,
    },
}

/// Result type returned by the package-owned distribution facade.
pub type DistributionResult<T = ()> = Result<T, DistributionError>;

/// Result of a completed distribution commit.
#[derive(Debug)]
pub struct CommitOutcome {
    cleanup: CleanupOutcome,
}

impl CommitOutcome {
    /// Returns the cleanup result after the installed distribution became
    /// authoritative.
    #[must_use]
    pub fn cleanup(&self) -> &CleanupOutcome {
        &self.cleanup
    }

    fn complete() -> Self {
        Self {
            cleanup: CleanupOutcome::Complete,
        }
    }

    fn backup_retained(backup: PathBuf, transaction: PathBuf, error: io::Error) -> Self {
        Self {
            cleanup: CleanupOutcome::BackupRetained {
                backup,
                transaction,
                error,
            },
        }
    }
}

/// Cleanup status returned after a successful installation.
#[derive(Debug)]
pub enum CleanupOutcome {
    /// The previous distribution and transaction directory were removed.
    Complete,
    /// The new distribution is installed, but the old backup remains for
    /// recovery because cleanup failed. The transaction journal is retained.
    BackupRetained {
        backup: PathBuf,
        transaction: PathBuf,
        error: io::Error,
    },
}

/// A closed-world distribution prepared for commit.
///
/// The root snapshot and every package-specific verification snapshot are
/// owned by this value. This makes verification evidence data carried by the
/// commit capability instead of callbacks supplied by the CLI.
#[derive(Debug)]
pub struct PreparedDistribution {
    root: crate::PreparedDirectoryCommit,
    packages: Vec<PreparedPackageVerification>,
}

#[derive(Debug)]
struct PreparedPackageVerification {
    relative_path: Option<PathBuf>,
    package: crate::PreparedPackageCommit,
}

impl PreparedDistribution {
    /// Creates a distribution from its closed-world root snapshot.
    #[must_use]
    pub fn new(root: crate::PreparedDirectoryCommit) -> Self {
        Self {
            root,
            packages: Vec::new(),
        }
    }

    /// Adds package evidence whose destination is the committed root itself.
    pub fn with_package(
        mut self,
        package: crate::PreparedPackageCommit,
    ) -> crate::PackageResult<Self> {
        if self
            .packages
            .iter()
            .any(|entry| entry.relative_path.is_none())
        {
            return Err("prepared distribution already has root package evidence".into());
        }
        self.packages.push(PreparedPackageVerification {
            relative_path: None,
            package,
        });
        Ok(self)
    }

    /// Adds package evidence at a validated path below the committed root.
    pub fn with_nested_package(
        mut self,
        relative_path: impl Into<PathBuf>,
        package: crate::PreparedPackageCommit,
    ) -> crate::PackageResult<Self> {
        let relative_path = relative_path.into();
        validate_distribution_relative_path(&relative_path)?;
        if self
            .packages
            .iter()
            .any(|entry| entry.relative_path.as_ref() == Some(&relative_path))
        {
            return Err(format!(
                "duplicate prepared distribution package path: {}",
                relative_path.display()
            )
            .into());
        }
        self.packages.push(PreparedPackageVerification {
            relative_path: Some(relative_path),
            package,
        });
        Ok(self)
    }

    /// Commits the prepared distribution using the platform file system.
    pub fn commit(self, destination: &Path) -> DistributionResult<CommitOutcome> {
        self.commit_with(destination, &SystemDistributionFileOps)
    }

    /// Commits the prepared distribution with injected file operations.
    ///
    /// This is primarily useful for package-level failure-injection tests;
    /// verification remains owned by the prepared evidence in all cases.
    pub fn commit_with(
        self,
        destination: &Path,
        file_ops: &impl DistributionFileOps,
    ) -> DistributionResult<CommitOutcome> {
        let root = &self.root;
        let packages = &self.packages;
        commit_prepared_directory_with(
            root,
            destination,
            |_| {
                for package in packages {
                    package.package.verify_source_contents()?;
                }
                Ok(())
            },
            |committed_root| {
                root.verify_committed_contents(committed_root)?;
                for package in packages {
                    let package_root = package.relative_path.as_deref().map_or_else(
                        || committed_root.to_path_buf(),
                        |path| committed_root.join(path),
                    );
                    package.package.verify_committed_contents(&package_root)?;
                }
                Ok(())
            },
            file_ops,
        )
    }
}

fn validate_distribution_relative_path(path: &Path) -> crate::PackageResult {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "prepared distribution package path must be a non-empty relative path: {}",
            path.display()
        )
        .into());
    }
    crate::validate_path_components(path)
}

#[derive(Debug)]
pub struct DistributionRecoveryError {
    pub destination: PathBuf,
    pub commit_error: io::Error,
    pub rollback_error: io::Error,
    pub recovery_path: PathBuf,
}

impl fmt::Display for DistributionRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to commit staged directory to {}: {}. Rollback also failed: {}. \
             The previous distribution was preserved at {}",
            self.destination.display(),
            self.commit_error,
            self.rollback_error,
            self.recovery_path.display()
        )
    }
}

impl std::error::Error for DistributionRecoveryError {}

#[derive(Debug)]
pub struct DistributionRecoveryQuarantineError {
    pub transaction: PathBuf,
    pub quarantine: Option<PathBuf>,
    pub reason: String,
}

impl fmt::Display for DistributionRecoveryQuarantineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(quarantine) = &self.quarantine {
            write!(
                formatter,
                "distribution transaction at {} was quarantined at {}: {}",
                self.transaction.display(),
                quarantine.display(),
                self.reason
            )
        } else {
            write!(
                formatter,
                "distribution transaction at {} could not be quarantined: {}",
                self.transaction.display(),
                self.reason
            )
        }
    }
}

impl std::error::Error for DistributionRecoveryQuarantineError {}

pub static TRANSACTION_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct DistributionTransactionDirectory {
    pub path: PathBuf,
    pub capability: Option<crate::PrivateStagingDirectory>,
}

impl DistributionTransactionDirectory {
    pub fn create(
        parent: &Path,
        destination_name: &str,
        destination_identity: Option<DirectoryIdentity>,
        file_ops: &impl DistributionFileOps,
    ) -> DistributionResult<(Self, TransactionJournal)> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..64 {
            let counter = TRANSACTION_COUNTER.fetch_add(1, Ordering::Relaxed);
            let transaction_id = format!("{}-{timestamp}-{counter}", std::process::id());
            let final_path =
                parent.join(format!(".{destination_name}.transaction-{transaction_id}"));
            let private_path = parent.join(format!(
                ".{destination_name}.transaction-private-{transaction_id}"
            ));
            if fs::symlink_metadata(&final_path).is_ok()
                || fs::symlink_metadata(&private_path).is_ok()
            {
                continue;
            }

            let capability = match crate::PrivateStagingDirectory::create(&private_path) {
                Ok(capability) => capability,
                Err(_error)
                    if fs::symlink_metadata(&private_path).is_ok()
                        || fs::symlink_metadata(&final_path).is_ok() =>
                {
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let parent_identity = directory_identity(parent)?;
            let transaction_identity = directory_identity(&private_path)?;
            let mut journal_state = TransactionJournal::new(
                transaction_id.clone(),
                destination_name.to_owned(),
                parent_identity,
                transaction_identity,
                destination_identity,
            )?;
            let journal = private_path.join(TRANSACTION_JOURNAL);
            if let Err(error) = write_transaction_state(
                &journal,
                parent,
                &mut journal_state,
                TransactionState::Prepared,
                file_ops,
            ) {
                drop(capability);
                return Err(error.into());
            }
            capability.verify()?;
            drop(capability);
            rename_path(&private_path, &final_path)?;
            sync_rename_parents(&private_path, &final_path, file_ops)?;
            let capability = crate::PrivateStagingDirectory::open(&final_path)?;
            return Ok((
                Self {
                    path: final_path,
                    capability: Some(capability),
                },
                journal_state,
            ));
        }
        Err(DistributionError::Invalid(
            "failed to allocate a unique distribution transaction directory".to_owned(),
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn verify(&self) -> DistributionResult<()> {
        self.capability
            .as_ref()
            .ok_or_else(|| {
                DistributionError::Invalid(
                    "distribution transaction capability was released".to_owned(),
                )
            })?
            .verify()?;
        Ok(())
    }

    pub fn keep(mut self) -> PathBuf {
        self.capability.take();
        std::mem::take(&mut self.path)
    }

    pub fn cleanup_now(
        mut self,
        parent: &Path,
        file_ops: &impl DistributionFileOps,
    ) -> io::Result<()> {
        self.capability.take();
        file_ops.remove_dir_all(&self.path)?;
        file_ops.sync_directory(parent)
    }
}

impl Drop for DistributionTransactionDirectory {
    fn drop(&mut self) {}
}

pub fn validate_commit_location(parent: &Path, destination: &Path) -> DistributionResult {
    crate::validate_directory_path(parent)?;
    validate_output_destination(destination)?;
    Ok(())
}

pub fn commit_prepared_directory(
    prepared: &crate::PreparedDirectoryCommit,
    destination: &Path,
    verify_source: impl Fn(&Path) -> DistributionResult,
    verify_destination: impl Fn(&Path) -> DistributionResult,
) -> DistributionResult<CommitOutcome> {
    commit_prepared_directory_with(
        prepared,
        destination,
        verify_source,
        verify_destination,
        &SystemDistributionFileOps,
    )
}

pub fn commit_prepared_directory_with(
    prepared: &crate::PreparedDirectoryCommit,
    destination: &Path,
    verify_source: impl Fn(&Path) -> DistributionResult,
    verify_destination: impl Fn(&Path) -> DistributionResult,
    file_ops: &impl DistributionFileOps,
) -> DistributionResult<CommitOutcome> {
    validate_output_destination(destination)?;
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    validate_commit_location(parent, destination)?;
    let destination_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            DistributionError::Invalid(
                "distribution destination name is not valid UTF-8".to_owned(),
            )
        })?;
    let commit_guard = DistributionCommitGuard::acquire(parent, destination_name)?;
    recover_stale_transactions(parent, destination_name, &commit_guard, file_ops)?;
    validate_commit_location(parent, destination)?;

    prepared.verify_source_contents()?;
    verify_source(prepared.staging_directory())?;

    let destination_identity = optional_directory_identity(destination)?;
    let (transaction, mut journal_state) = DistributionTransactionDirectory::create(
        parent,
        destination_name,
        destination_identity,
        file_ops,
    )?;
    transaction.verify()?;
    let previous = transaction.path().join("previous");
    let journal = transaction.path().join(TRANSACTION_JOURNAL);
    validate_commit_location(parent, destination)?;
    let had_previous = destination_identity.is_some();
    if had_previous {
        validate_commit_location(parent, destination)?;
        commit_guard.ensure_held()?;
        file_ops.rename(destination, &previous)?;
        sync_rename_parents(destination, &previous, file_ops)?;
        journal_state.previous_identity = Some(directory_identity(&previous)?);
        write_transaction_state(
            &journal,
            parent,
            &mut journal_state,
            TransactionState::PreviousSaved,
            file_ops,
        )?;
    } else {
        write_transaction_state(
            &journal,
            parent,
            &mut journal_state,
            TransactionState::NoPrevious,
            file_ops,
        )?;
    }
    prepared.verify_source_contents()?;
    verify_source(prepared.staging_directory())?;
    validate_commit_location(parent, destination)?;
    journal_state.installed_identity = Some(directory_identity(prepared.staging_directory())?);
    write_transaction_state(
        &journal,
        parent,
        &mut journal_state,
        if had_previous {
            TransactionState::InstallPending
        } else {
            TransactionState::InstallPendingNoPrevious
        },
        file_ops,
    )?;
    commit_guard.ensure_held()?;
    // The lease excludes new writers while the final source identity and
    // contents are verified. Mandatory exclusion of a process that already
    // owns a writable handle is not available through portable Rust file APIs;
    // the staged tree remains private and post-commit verification remains the
    // integrity check for that out-of-scope case.
    let source_lease = prepared.lock_source_for_commit()?;
    prepared.verify_source_contents()?;
    verify_source(prepared.staging_directory())?;
    commit_guard.ensure_held()?;
    #[cfg(target_os = "windows")]
    // Windows rejects renaming a non-empty directory while a descendant file
    // has an open handle, even when that handle permits delete sharing. The
    // private staging tree has just passed its final identity/content check,
    // and the transaction lock remains held across publication.
    drop(source_lease);
    if let Err(commit_error) = file_ops.rename(prepared.staging_directory(), destination) {
        let rollback_state = if had_previous {
            TransactionState::RollbackPending
        } else {
            TransactionState::RollbackPendingNoPrevious
        };
        if let Err(state_error) = write_transaction_state(
            &journal,
            parent,
            &mut journal_state,
            rollback_state,
            file_ops,
        ) {
            return Err(DistributionError::StateRecordingFailure {
                phase: "distribution commit",
                primary: Box::new(DistributionError::Io(commit_error)),
                state: Box::new(state_error.into()),
            });
        }
        if had_previous {
            let expected_previous = journal_state
                .previous_identity
                .ok_or_else(|| {
                    DistributionError::Invalid(
                        "transaction has no previous destination identity".to_owned(),
                    )
                })
                .map_err(error_as_io);
            let rollback = validate_commit_location(parent, destination)
                .map_err(error_as_io)
                .and_then(|_| ensure_destination_absent(destination).map_err(error_as_io))
                .and(expected_previous)
                .and_then(|expected| {
                    require_directory_identity(&previous, expected, "previous backup")
                        .map_err(error_as_io)
                })
                .and_then(|_| file_ops.rename(&previous, destination))
                .and_then(|_| sync_rename_parents(&previous, destination, file_ops))
                .and_then(|_| {
                    let expected = journal_state
                        .previous_identity
                        .ok_or_else(|| {
                            DistributionError::Invalid(
                                "transaction has no previous destination identity".to_owned(),
                            )
                        })
                        .map_err(error_as_io)?;
                    require_directory_identity(destination, expected, "restored destination")
                        .map_err(error_as_io)
                });
            if let Err(rollback_error) = rollback {
                return Err(distribution_recovery_error(
                    transaction,
                    destination,
                    commit_error,
                    rollback_error,
                )
                .into());
            }
            let state_result = write_transaction_state(
                &journal,
                parent,
                &mut journal_state,
                TransactionState::RolledBack,
                file_ops,
            );
            if let Err(state_error) = state_result {
                return Err(DistributionError::StateRecordingFailure {
                    phase: "distribution commit rollback",
                    primary: Box::new(DistributionError::Io(commit_error)),
                    state: Box::new(state_error.into()),
                });
            }
            transaction.cleanup_now(parent, file_ops)?;
        }
        file_ops.sync_directory(parent)?;
        return Err(DistributionError::Io(commit_error));
    }
    sync_rename_parents(prepared.staging_directory(), destination, file_ops)?;
    #[cfg(not(target_os = "windows"))]
    drop(source_lease);
    journal_state.installed_identity = Some(directory_identity(destination)?);
    write_transaction_state(
        &journal,
        parent,
        &mut journal_state,
        if had_previous {
            TransactionState::Installed
        } else {
            TransactionState::InstalledNoPrevious
        },
        file_ops,
    )?;

    if let Err(verification_error) = validate_output_destination(destination)
        .map_err(DistributionError::from)
        .and_then(|_| verify_destination(destination))
    {
        let rollback_state = if had_previous {
            TransactionState::RollbackPending
        } else {
            TransactionState::RollbackPendingNoPrevious
        };
        if let Err(state_error) = write_transaction_state(
            &journal,
            parent,
            &mut journal_state,
            rollback_state,
            file_ops,
        ) {
            return Err(DistributionError::StateRecordingFailure {
                phase: "post-commit verification",
                primary: Box::new(verification_error),
                state: Box::new(state_error.into()),
            });
        }
        if had_previous {
            let failed = transaction.path().join("failed-install");
            let expected_installed = journal_state.installed_identity.ok_or_else(|| {
                DistributionError::Invalid(
                    "transaction has no installed destination identity".to_owned(),
                )
            })?;
            let failed_install = validate_commit_location(parent, destination)
                .map_err(error_as_io)
                .and_then(|_| {
                    require_directory_identity(
                        destination,
                        expected_installed,
                        "installed destination",
                    )
                    .map_err(error_as_io)
                })
                .and_then(|_| file_ops.rename(destination, &failed))
                .and_then(|_| sync_rename_parents(destination, &failed, file_ops));
            if let Err(rollback_error) = failed_install {
                return Err(distribution_recovery_error(
                    transaction,
                    destination,
                    error_as_io(verification_error),
                    rollback_error,
                )
                .into());
            }
            let expected_previous = journal_state
                .previous_identity
                .or(journal_state.destination_identity)
                .ok_or_else(|| {
                    DistributionError::Invalid(
                        "transaction has no previous destination identity".to_owned(),
                    )
                })?;
            let rollback = validate_commit_location(parent, destination)
                .map_err(error_as_io)
                .and_then(|_| ensure_destination_absent(destination).map_err(error_as_io))
                .and_then(|_| {
                    require_directory_identity(&previous, expected_previous, "previous backup")
                        .map_err(error_as_io)
                })
                .and_then(|_| file_ops.rename(&previous, destination))
                .and_then(|_| sync_rename_parents(&previous, destination, file_ops))
                .and_then(|_| {
                    require_directory_identity(
                        destination,
                        expected_previous,
                        "restored destination",
                    )
                    .map_err(error_as_io)
                });
            if let Err(rollback_error) = rollback {
                return Err(distribution_recovery_error(
                    transaction,
                    destination,
                    error_as_io(verification_error),
                    rollback_error,
                )
                .into());
            }
            if let Err(state_error) = write_transaction_state(
                &journal,
                parent,
                &mut journal_state,
                TransactionState::RolledBack,
                file_ops,
            ) {
                return Err(DistributionError::StateRecordingFailure {
                    phase: "post-commit verification rollback",
                    primary: Box::new(verification_error),
                    state: Box::new(state_error.into()),
                });
            }
            remove_installed_distribution_with(&failed, &journal_state, file_ops)?;
            transaction.cleanup_now(parent, file_ops)?;
        } else if let Err(rollback_error) = validate_commit_location(parent, destination)
            .map_err(error_as_io)
            .and_then(|_| {
                remove_installed_distribution_with(destination, &journal_state, file_ops)
                    .map_err(error_as_io)
            })
        {
            return Err(distribution_recovery_error(
                transaction,
                destination,
                error_as_io(verification_error),
                rollback_error,
            )
            .into());
        } else {
            transaction.cleanup_now(parent, file_ops)?;
        }
        return Err(verification_error);
    }

    write_transaction_state(
        &journal,
        parent,
        &mut journal_state,
        TransactionState::Committed,
        file_ops,
    )?;
    if had_previous {
        if let Err(error) = file_ops.remove_dir_all(&previous) {
            let transaction_path = transaction.path().to_path_buf();
            let _ = transaction.keep();
            return Ok(CommitOutcome::backup_retained(
                previous,
                transaction_path,
                error,
            ));
        }
        file_ops.sync_directory(transaction.path())?;
    }
    transaction.cleanup_now(parent, file_ops)?;
    Ok(CommitOutcome::complete())
}

#[derive(Debug)]
pub struct IoErrorSource(pub DistributionError);

impl fmt::Display for IoErrorSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for IoErrorSource {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

pub fn error_as_io<E>(error: E) -> io::Error
where
    E: Into<DistributionError>,
{
    // Keep the typed distribution error behind the standard I/O error rather
    // than flattening it to a display string.
    io::Error::other(IoErrorSource(error.into()))
}

pub fn distribution_recovery_error(
    transaction: DistributionTransactionDirectory,
    destination: &Path,
    commit_error: io::Error,
    rollback_error: io::Error,
) -> DistributionRecoveryError {
    let recovery_root = transaction.keep();
    DistributionRecoveryError {
        destination: destination.to_path_buf(),
        commit_error,
        rollback_error,
        recovery_path: recovery_root.join("previous"),
    }
}

pub fn quarantine_transaction(
    parent: &Path,
    destination_name: &str,
    prefix: &str,
    transaction: &Path,
    reason: impl Into<String>,
    file_ops: &impl DistributionFileOps,
) -> DistributionResult {
    let reason = reason.into();
    let id = transaction_id(transaction, prefix).unwrap_or_else(|_| "unknown".to_owned());
    for _ in 0..64 {
        let counter = TRANSACTION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let quarantine = parent.join(format!(
            ".{destination_name}.quarantine-transaction-{id}-{counter}"
        ));
        if fs::symlink_metadata(&quarantine).is_ok() {
            continue;
        }
        if let Err(error) = file_ops.rename(transaction, &quarantine) {
            return Err(DistributionRecoveryQuarantineError {
                transaction: transaction.to_path_buf(),
                quarantine: None,
                reason: format!("{reason}; quarantine rename failed: {error}"),
            }
            .into());
        }
        if let Err(error) = file_ops.sync_directory(parent) {
            return Err(DistributionRecoveryQuarantineError {
                transaction: transaction.to_path_buf(),
                quarantine: Some(quarantine),
                reason: format!("{reason}; quarantine directory sync failed: {error}"),
            }
            .into());
        }
        return Err(DistributionRecoveryQuarantineError {
            transaction: transaction.to_path_buf(),
            quarantine: Some(quarantine),
            reason,
        }
        .into());
    }
    Err(DistributionRecoveryQuarantineError {
        transaction: transaction.to_path_buf(),
        quarantine: None,
        reason: format!("{reason}; could not allocate a quarantine name"),
    }
    .into())
}

pub fn recover_stale_transactions(
    parent: &Path,
    destination_name: &str,
    commit_guard: &DistributionCommitGuard,
    file_ops: &impl DistributionFileOps,
) -> DistributionResult {
    commit_guard.ensure_held()?;
    if commit_guard.destination() != parent.join(destination_name) {
        return Err(DistributionError::Invalid(
            "distribution recovery lock does not match its destination".to_owned(),
        ));
    }
    crate::validate_directory_path(parent)?;
    let prefix = format!(".{destination_name}.transaction-");
    let destination = parent.join(destination_name);
    let transactions = fs::read_dir(parent)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix))
        })
        .collect::<Vec<_>>();

    for transaction in transactions {
        commit_guard.ensure_held()?;
        crate::validate_path_components(&transaction)?;
        let private_transaction = is_private_transaction(&transaction, &prefix)?;
        let payloads = transaction_payloads(&transaction)?;
        let journal = transaction.join(TRANSACTION_JOURNAL);
        let next = transaction.join("journal.next");
        crate::validate_path_components(&journal)?;
        crate::validate_path_components(&next)?;

        let journal_metadata = match fs::symlink_metadata(&journal) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let mut next_metadata = match fs::symlink_metadata(&next) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };

        let mut journal_state = if let Some(metadata) = journal_metadata.as_ref() {
            if !metadata.is_file() {
                return quarantine_transaction(
                    parent,
                    destination_name,
                    &prefix,
                    &transaction,
                    "transaction journal is not a regular file",
                    file_ops,
                );
            }
            match read_transaction_journal(&journal) {
                Ok(state) => state,
                Err(error) => {
                    return quarantine_transaction(
                        parent,
                        destination_name,
                        &prefix,
                        &transaction,
                        format!("transaction journal is invalid: {error}"),
                        file_ops,
                    );
                }
            }
        } else if let Some(metadata) = next_metadata.as_ref() {
            if !metadata.is_file() {
                return quarantine_transaction(
                    parent,
                    destination_name,
                    &prefix,
                    &transaction,
                    "journal.next is not a regular file",
                    file_ops,
                );
            }
            let next_state = match read_transaction_journal(&next) {
                Ok(state) => state,
                Err(_error) if payloads.is_empty() => {
                    fs::remove_file(&next)?;
                    file_ops.sync_directory(&transaction)?;
                    remove_empty_transaction(parent, &transaction, file_ops)?;
                    continue;
                }
                Err(error) => {
                    return quarantine_transaction(
                        parent,
                        destination_name,
                        &prefix,
                        &transaction,
                        format!("journal.next is invalid: {error}"),
                        file_ops,
                    );
                }
            };
            let transaction_directory = validate_transaction_provenance(
                parent,
                destination_name,
                &prefix,
                &transaction,
                &next_state,
            )?;
            transaction_directory.verify()?;
            atomic_replace_file(&next, &journal)?;
            file_ops.sync_directory(&transaction)?;
            file_ops.sync_directory(parent)?;
            next_metadata = None;
            next_state
        } else if payloads.is_empty() {
            remove_empty_transaction(parent, &transaction, file_ops)?;
            continue;
        } else {
            return quarantine_transaction(
                parent,
                destination_name,
                &prefix,
                &transaction,
                "transaction journal is missing while recovery payloads remain",
                file_ops,
            );
        };

        let transaction_directory = validate_transaction_provenance(
            parent,
            destination_name,
            &prefix,
            &transaction,
            &journal_state,
        )?;

        if let Some(metadata) = next_metadata.as_ref() {
            if metadata.is_file() {
                match read_transaction_journal(&next) {
                    Ok(next_state) if next_state.sequence > journal_state.sequence => {
                        validate_transaction_provenance(
                            parent,
                            destination_name,
                            &prefix,
                            &transaction,
                            &next_state,
                        )?
                        .verify()?;
                        atomic_replace_file(&next, &journal)?;
                        file_ops.sync_directory(&transaction)?;
                        file_ops.sync_directory(parent)?;
                        journal_state = next_state;
                    }
                    Ok(_) | Err(_) => {
                        fs::remove_file(&next)?;
                        file_ops.sync_directory(&transaction)?;
                    }
                }
            } else {
                return quarantine_transaction(
                    parent,
                    destination_name,
                    &prefix,
                    &transaction,
                    "journal.next is not a regular file",
                    file_ops,
                );
            }
        }

        if private_transaction && !payloads.is_empty() {
            return quarantine_transaction(
                parent,
                destination_name,
                &prefix,
                &transaction,
                "private transaction directory contains recovery payloads",
                file_ops,
            );
        }

        let previous = transaction.join("previous");
        crate::validate_path_components(&previous)?;
        commit_guard.ensure_held()?;
        match journal_state.state {
            TransactionState::Prepared => {
                if optional_directory_identity(&previous)?.is_some() {
                    let expected_previous = journal_state
                        .previous_identity
                        .or(journal_state.destination_identity)
                        .ok_or_else(|| {
                            DistributionError::Invalid(
                                "prepared transaction has no previous identity".to_owned(),
                            )
                        })?;
                    require_directory_identity(&previous, expected_previous, "previous backup")?;
                    if optional_directory_identity(&destination)?.is_some() {
                        return Err(DistributionError::Invalid(format!(
                            "distribution transaction journal is inconsistent: {}",
                            transaction.display()
                        )));
                    }
                    file_ops.rename(&previous, &destination)?;
                    sync_rename_parents(&previous, &destination, file_ops)?;
                    require_directory_identity(
                        &destination,
                        expected_previous,
                        "restored destination",
                    )?;
                } else if let Some(expected) = journal_state.destination_identity {
                    require_directory_identity(&destination, expected, "original destination")?;
                }
            }
            TransactionState::NoPrevious => {
                if optional_directory_identity(&previous)?.is_some() {
                    return Err(DistributionError::Invalid(format!(
                        "distribution transaction journal is inconsistent: {}",
                        transaction.display()
                    )));
                }
                if optional_directory_identity(&destination)?.is_some()
                    && journal_state.destination_identity.is_none()
                {
                    return Err(DistributionError::Invalid(format!(
                        "no-previous transaction has an unexpected destination: {}",
                        transaction.display()
                    )));
                }
                if let Some(expected) = journal_state.destination_identity {
                    require_directory_identity(&destination, expected, "original destination")?;
                }
            }
            TransactionState::InstallPending => {
                if optional_directory_identity(&previous)?.is_some() {
                    restore_previous_distribution(
                        &destination,
                        &transaction,
                        &previous,
                        &journal_state,
                        file_ops,
                    )?;
                } else {
                    return Err(DistributionError::Invalid(format!(
                        "install-pending transaction lost its backup: {}",
                        transaction.display()
                    )));
                }
            }
            TransactionState::InstallPendingNoPrevious => {
                if optional_directory_identity(&previous)?.is_some() {
                    return Err(DistributionError::Invalid(format!(
                        "distribution transaction journal is inconsistent: {}",
                        transaction.display()
                    )));
                }
                remove_installed_distribution_with(&destination, &journal_state, file_ops)?;
                remove_recovery_install(&transaction, &journal_state, file_ops)?;
            }
            TransactionState::PreviousSaved => {
                if optional_directory_identity(&previous)?.is_some() {
                    restore_previous_distribution(
                        &destination,
                        &transaction,
                        &previous,
                        &journal_state,
                        file_ops,
                    )?;
                } else {
                    return Err(DistributionError::Invalid(format!(
                        "previous-saved transaction lost its backup: {}",
                        transaction.display()
                    )));
                }
            }
            TransactionState::Installed | TransactionState::RollbackPending => {
                if optional_directory_identity(&previous)?.is_some() {
                    restore_previous_distribution(
                        &destination,
                        &transaction,
                        &previous,
                        &journal_state,
                        file_ops,
                    )?;
                } else if journal_state.state == TransactionState::RollbackPending {
                    require_original_destination(&destination, &journal_state)?;
                    remove_recovery_install(&transaction, &journal_state, file_ops)?;
                } else {
                    return Err(DistributionError::Invalid(format!(
                        "installed transaction lost its backup: {}",
                        transaction.display()
                    )));
                }
            }
            TransactionState::InstalledNoPrevious | TransactionState::RollbackPendingNoPrevious => {
                if optional_directory_identity(&previous)?.is_some() {
                    return Err(DistributionError::Invalid(format!(
                        "distribution transaction journal is inconsistent: {}",
                        transaction.display()
                    )));
                }
                remove_installed_distribution_with(&destination, &journal_state, file_ops)?;
                remove_recovery_install(&transaction, &journal_state, file_ops)?;
            }
            TransactionState::RolledBack => {
                if optional_directory_identity(&previous)?.is_some() {
                    restore_previous_distribution(
                        &destination,
                        &transaction,
                        &previous,
                        &journal_state,
                        file_ops,
                    )?;
                } else {
                    require_original_destination(&destination, &journal_state)?;
                }
                remove_recovery_install(&transaction, &journal_state, file_ops)?;
            }
            TransactionState::Committed => {
                let installed = journal_state.installed_identity.ok_or_else(|| {
                    DistributionError::Invalid(
                        "committed transaction has no installed identity".to_owned(),
                    )
                })?;
                if optional_directory_identity(&destination)?.is_none() {
                    // Committed means the installed directory is authoritative.
                    // The old distribution is cleanup payload only and must
                    // never become a recovery source after commit.
                    return quarantine_transaction(
                        parent,
                        destination_name,
                        &prefix,
                        &transaction,
                        "committed destination is missing; refusing to restore the previous distribution",
                        file_ops,
                    );
                }
                require_directory_identity(&destination, installed, "installed destination")?;
                if optional_directory_identity(&previous)?.is_some() {
                    let expected = journal_state.previous_identity.ok_or_else(|| {
                        DistributionError::Invalid(
                            "committed transaction has no previous identity".to_owned(),
                        )
                    })?;
                    require_directory_identity(&previous, expected, "committed backup")?;
                    file_ops.remove_dir_all(&previous)?;
                    file_ops.sync_directory(&transaction)?;
                }
                remove_recovery_install(&transaction, &journal_state, file_ops)?;
            }
        }
        commit_guard.ensure_held()?;
        transaction_directory.verify()?;
        file_ops.remove_dir_all(&transaction)?;
        file_ops.sync_directory(parent)?;
    }
    Ok(())
}

pub fn require_original_destination(
    destination: &Path,
    journal: &TransactionJournal,
) -> DistributionResult<()> {
    let expected = journal.destination_identity.ok_or_else(|| {
        DistributionError::Invalid("transaction has no original destination identity".to_owned())
    })?;
    Ok(require_directory_identity(
        destination,
        expected,
        "restored destination",
    )?)
}

pub fn remove_installed_distribution_with(
    destination: &Path,
    journal: &TransactionJournal,
    file_ops: &impl DistributionFileOps,
) -> DistributionResult<()> {
    if optional_directory_identity(destination)?.is_none() {
        return Ok(());
    }
    let expected = journal.installed_identity.ok_or_else(|| {
        DistributionError::Invalid("transaction has no installed destination identity".to_owned())
    })?;
    require_directory_identity(destination, expected, "installed destination")?;
    file_ops.remove_dir_all(destination)?;
    file_ops.sync_directory(
        destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?;
    Ok(())
}

pub fn restore_previous_distribution(
    destination: &Path,
    transaction: &Path,
    previous: &Path,
    journal: &TransactionJournal,
    file_ops: &impl DistributionFileOps,
) -> DistributionResult {
    validate_output_destination(destination)?;
    let expected_previous = journal
        .previous_identity
        .or(journal.destination_identity)
        .ok_or_else(|| {
            DistributionError::Invalid(
                "transaction has no previous destination identity".to_owned(),
            )
        })?;
    require_directory_identity(previous, expected_previous, "previous backup")?;
    let recovery_install = transaction.join("recovery-install");
    crate::validate_path_components(&recovery_install)?;
    if optional_directory_identity(destination)?.is_some() {
        let expected_installed = journal.installed_identity.ok_or_else(|| {
            DistributionError::Invalid(
                "transaction has no installed destination identity".to_owned(),
            )
        })?;
        require_directory_identity(destination, expected_installed, "installed destination")?;
        if optional_directory_identity(&recovery_install)?.is_some() {
            require_directory_identity(
                &recovery_install,
                expected_installed,
                "recovery installation",
            )?;
            file_ops.remove_dir_all(&recovery_install)?;
            file_ops.sync_directory(transaction)?;
        }
        file_ops.rename(destination, &recovery_install)?;
        sync_rename_parents(destination, &recovery_install, file_ops)?;
        require_directory_identity(previous, expected_previous, "previous backup")?;
        file_ops.rename(previous, destination)?;
        sync_rename_parents(previous, destination, file_ops)?;
        require_directory_identity(destination, expected_previous, "restored destination")?;
        file_ops.remove_dir_all(&recovery_install)?;
        file_ops.sync_directory(transaction)?;
    } else {
        if optional_directory_identity(&recovery_install)?.is_some() {
            let expected_installed = journal.installed_identity.ok_or_else(|| {
                DistributionError::Invalid("transaction has no installed identity".to_owned())
            })?;
            require_directory_identity(
                &recovery_install,
                expected_installed,
                "recovery installation",
            )?;
        }
        file_ops.rename(previous, destination)?;
        sync_rename_parents(previous, destination, file_ops)?;
        require_directory_identity(destination, expected_previous, "restored destination")?;
        if optional_directory_identity(&recovery_install)?.is_some() {
            file_ops.remove_dir_all(&recovery_install)?;
            file_ops.sync_directory(transaction)?;
        }
    }
    Ok(())
}

pub fn remove_recovery_install(
    transaction: &Path,
    journal: &TransactionJournal,
    file_ops: &impl DistributionFileOps,
) -> DistributionResult {
    let recovery_install = transaction.join("recovery-install");
    crate::validate_path_components(&recovery_install)?;
    if optional_directory_identity(&recovery_install)?.is_some() {
        let expected = journal.installed_identity.ok_or_else(|| {
            DistributionError::Invalid(
                "transaction has no installed identity for recovery installation".to_owned(),
            )
        })?;
        require_directory_identity(&recovery_install, expected, "recovery installation")?;
        file_ops.remove_dir_all(&recovery_install)?;
        file_ops.sync_directory(transaction)?;
    }
    Ok(())
}
