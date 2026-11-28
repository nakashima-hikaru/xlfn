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
use std::io;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod journal_tests;
#[cfg(test)]
mod transaction_tests;

mod journal;
mod managed;
use journal::{
    TRANSACTION_JOURNAL, TransactionJournal, TransactionState, read_transaction_journal,
    validate_transaction_provenance, write_transaction_state,
};
use managed::ManagedOutput;
pub use managed::PackageOutputLock;

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
    // The destination name is also used in transaction directory and lock
    // basenames. Host ancestors are independent of those generated names.
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("distribution destination must have a UTF-8 basename")?;
    crate::validate_windows_basename(name)?;
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
struct DistributionCommitGuard {
    lock_file: std::fs::File,
    lock_path: PathBuf,
    #[cfg(unix)]
    lock_identity: LockFileIdentity,
    destination: PathBuf,
}

impl DistributionCommitGuard {
    /// Acquires the destination lock below `parent`.
    fn acquire(parent: &Path, destination_name: &str) -> PackageResult<Self> {
        let lock_path = parent.join(format!(".{destination_name}.lock"));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;

            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
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
        let metadata = lock_file.metadata()?;
        if !metadata.is_file() || crate::is_reparse_point(&metadata) {
            return Err("distribution commit lock handle is not a regular file".into());
        }

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
    fn ensure_held(&self) -> io::Result<()> {
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
    fn destination(&self) -> &Path {
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
fn rename_path(from: &Path, to: &Path) -> io::Result<()> {
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
fn atomic_replace_file(from: &Path, to: &Path) -> io::Result<()> {
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
#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    let directory = std::fs::File::open(path)?;
    directory.sync_all()
}

/// Flushes a directory entry where the host platform supports it.
#[cfg(target_os = "windows")]
fn sync_directory(path: &Path) -> io::Result<()> {
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

/// Flushes a directory entry where the host platform supports it.
#[cfg(not(any(unix, target_os = "windows")))]
fn sync_directory(_: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "directory synchronization is unsupported on this platform",
    ))
}

/// Syncs both directory entries affected by a rename.
fn sync_rename_parents(
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

#[cfg(any(target_os = "windows", test))]
fn retryable_windows_path_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
    ) || matches!(error.raw_os_error(), Some(5) | Some(32) | Some(33))
}

#[cfg(target_os = "windows")]
fn retry_windows_path_operation(operation: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    retry_path_operation(operation, std::thread::sleep)
}

#[cfg(any(target_os = "windows", test))]
fn retry_path_operation(
    mut operation: impl FnMut() -> io::Result<()>,
    mut sleep: impl FnMut(std::time::Duration),
) -> io::Result<()> {
    const ATTEMPTS: usize = 24;
    let mut delay = std::time::Duration::from_millis(10);
    let mut result = operation();
    // Every iteration owns the preceding attempt's actual outcome. The final
    // outcome is returned directly, even when the retry budget is exhausted.
    for _ in 1..ATTEMPTS {
        match result {
            Err(error) if retryable_windows_path_error(&error) => {
                sleep(delay);
                delay = delay
                    .saturating_mul(2)
                    .min(std::time::Duration::from_millis(500));
                // Preserve the original attempt's resource/error drop order:
                // finish backoff and release it before starting another I/O.
                drop(error);
            }
            result => return result,
        }
        result = operation();
    }
    result
}

#[cfg(target_os = "windows")]
fn move_file_ex_with_retry(from: &Path, to: &Path, flags: u32) -> io::Result<()> {
    use crate::win32::MoveFileExW;

    let from_wide = crate::windows_path_wide(from)?;
    let to_wide = crate::windows_path_wide(to)?;
    retry_windows_path_operation(|| {
        // SAFETY: both paths are live, NUL-terminated buffers for this call.
        if unsafe { MoveFileExW(from_wide.as_ptr(), to_wide.as_ptr(), flags) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

/// Returns the identity of an existing directory, or `None` when it is absent.
fn optional_directory_identity(path: &Path) -> PackageResult<Option<DirectoryIdentity>> {
    match fs::symlink_metadata(path) {
        Ok(_) => crate::directory_identity(path).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Requires a directory to retain a previously recorded identity.
fn require_directory_identity(
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

/// Yields transaction payload paths while excluding journal control files.
fn transaction_payloads(
    transaction: &Path,
) -> io::Result<impl Iterator<Item = io::Result<PathBuf>> + use<>> {
    Ok(fs::read_dir(transaction)?
        .map(|entry| entry.map(|entry| entry.path()))
        .filter(|entry| match entry {
            Ok(path) => !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(TRANSACTION_JOURNAL) | Some("journal.next")
            ),
            Err(_) => true,
        }))
}

/// Extracts a transaction identifier from its canonical directory name.
fn transaction_id(transaction: &Path, prefix: &str) -> PackageResult<String> {
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
fn is_private_transaction(transaction: &Path, prefix: &str) -> PackageResult<bool> {
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

/// Requires a destination path to be absent.
fn ensure_destination_absent(destination: &Path) -> PackageResult {
    match fs::symlink_metadata(destination) {
        Ok(_) => Err(format!("destination unexpectedly exists: {}", destination.display()).into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Removes an empty transaction directory after validating its private identity.
fn remove_empty_transaction(
    parent: &Path,
    transaction: &Path,
    file_ops: &impl DistributionFileOps,
) -> PackageResult {
    let transaction_directory = PrivateStagingDirectory::open(transaction)?;
    transaction_directory.verify()?;
    drop(transaction_directory);
    file_ops.remove_dir_all(transaction)?;
    file_ops.sync_directory(parent)?;
    Ok(())
}

fn cleanup_unmutated_transaction(
    parent: &Path,
    transaction: &Path,
    identity: DirectoryIdentity,
    file_ops: &impl DistributionFileOps,
) -> PackageResult {
    let directory = PrivateStagingDirectory::open(transaction)?;
    directory.verify()?;
    if directory.identity != identity
        || transaction_payloads(transaction)?
            .next()
            .transpose()?
            .is_some()
    {
        return Err(format!(
            "transaction cannot be cleaned before publication: {}",
            transaction.display()
        )
        .into());
    }
    for name in [TRANSACTION_JOURNAL, "journal.next"] {
        let path = transaction.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !crate::is_reparse_point(&metadata) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(_) => {
                return Err(format!(
                    "transaction contains an unexpected journal entry: {}",
                    path.display()
                )
                .into());
            }
        }
    }
    drop(directory);
    file_ops.remove_dir_all(transaction)?;
    file_ops.sync_directory(parent)?;
    Ok(())
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
    /// A commit failed after changing the destination's namespace, or a
    /// retained transaction could not be recovered. The destination may contain
    /// the old package, the new package, or no package.
    /// These paths identify the transaction and possible previous package;
    /// cleanup may already have removed them. Recovery must follow the durable
    /// journal rather than treating the previous package as authoritative.
    #[error(
        "distribution commit interrupted for {destination}: {source}; transaction location: {transaction}"
    )]
    Interrupted {
        destination: PathBuf,
        transaction: PathBuf,
        previous: Option<PathBuf>,
        #[source]
        source: Box<DistributionError>,
    },
    #[error("{phase}: {primary}; cleanup failed: {cleanup}")]
    CleanupFailure {
        phase: &'static str,
        primary: Box<DistributionError>,
        #[source]
        cleanup: Box<DistributionError>,
    },
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
    /// The new distribution is authoritative, but the old backup remains as
    /// cleanup payload. The transaction journal is retained; later commits
    /// retry its cleanup and can proceed while the validated backup is locked.
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
        self.commit_with_policy(destination, file_ops, None)
    }

    fn commit_with_policy(
        self,
        destination: &Path,
        file_ops: &impl DistributionFileOps,
        managed: Option<ManagedOutput<'_>>,
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
            managed,
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

/// A commit and its rollback both failed to complete. The retained journal
/// determines recovery; the destination may already contain the restored
/// package even though recording or persisting the rollback failed.
#[derive(Debug)]
pub struct DistributionRecoveryError {
    pub destination: PathBuf,
    pub commit_error: io::Error,
    pub rollback_error: io::Error,
    /// Candidate location of the previous package in the retained transaction.
    /// It may be absent after a successful restore rename or an initial install,
    /// and must not be treated as authoritative without consulting the journal.
    pub recovery_path: PathBuf,
}

impl fmt::Display for DistributionRecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to commit staged directory to {}: {}. Rollback also failed: {}. \
             Possible previous distribution location: {}. \
             Inspect the destination and retained transaction journal before recovery",
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

static TRANSACTION_COUNTER: AtomicU64 = AtomicU64::new(0);

struct DistributionTransactionDirectory {
    path: PathBuf,
    capability: Option<crate::PrivateStagingDirectory>,
}

impl DistributionTransactionDirectory {
    fn create(
        parent: &Path,
        destination_name: &str,
        destination_identity: Option<DirectoryIdentity>,
        file_ops: &impl DistributionFileOps,
    ) -> DistributionResult<(Self, TransactionJournal)> {
        Self::create_with_staging(
            parent,
            destination_name,
            destination_identity,
            file_ops,
            crate::PrivateStagingDirectory::create,
        )
    }

    fn create_with_staging(
        parent: &Path,
        destination_name: &str,
        destination_identity: Option<DirectoryIdentity>,
        file_ops: &impl DistributionFileOps,
        create_staging: impl Fn(&Path) -> PackageResult<PrivateStagingDirectory>,
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

            let capability = match create_staging(&private_path) {
                Ok(capability) => capability,
                Err(crate::PackageError::Io(error))
                    if error.kind() == io::ErrorKind::AlreadyExists =>
                {
                    continue;
                }
                // Once create succeeded, an open/ACL/identity failure is not a
                // collision. Retrying would leave up to 64 private directories
                // and replace the useful initialization error with exhaustion.
                Err(error) => return Err(error.into()),
            };
            let transaction_identity = capability.identity;
            let mut path = private_path.clone();
            let result = (|| -> DistributionResult<_> {
                let mut journal_state = TransactionJournal::new(
                    transaction_id,
                    destination_name.to_owned(),
                    directory_identity(parent)?,
                    transaction_identity,
                    destination_identity,
                )?;
                write_transaction_state(
                    &private_path.join(TRANSACTION_JOURNAL),
                    parent,
                    &mut journal_state,
                    TransactionState::Prepared,
                    file_ops,
                )?;
                capability.verify()?;
                drop(capability);
                rename_path(&private_path, &final_path)?;
                path = final_path;
                sync_rename_parents(&private_path, &path, file_ops)?;
                let capability = PrivateStagingDirectory::open(&path)?;
                if capability.identity != transaction_identity {
                    return Err(DistributionError::Invalid(
                        "distribution transaction directory identity changed".to_owned(),
                    ));
                }
                Ok((capability, journal_state))
            })();
            return match result {
                Ok((capability, journal)) => Ok((
                    Self {
                        path,
                        capability: Some(capability),
                    },
                    journal,
                )),
                Err(primary) => Err(cleanup_after_unmutated_failure(
                    &parent.join(destination_name),
                    &path,
                    primary,
                    cleanup_unmutated_transaction(parent, &path, transaction_identity, file_ops),
                )),
            };
        }
        Err(DistributionError::Invalid(
            "failed to allocate a unique distribution transaction directory".to_owned(),
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn verify(&self) -> DistributionResult<()> {
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

    fn keep(&mut self) -> PathBuf {
        self.capability.take();
        std::mem::take(&mut self.path)
    }

    fn cleanup_now(
        &mut self,
        parent: &Path,
        file_ops: &impl DistributionFileOps,
    ) -> io::Result<()> {
        self.capability.take();
        file_ops.remove_dir_all(&self.path)?;
        file_ops.sync_directory(parent)
    }
}

fn validate_commit_location(parent: &Path, destination: &Path) -> DistributionResult {
    crate::validate_directory_path(parent)?;
    validate_output_destination(destination)?;
    Ok(())
}

fn commit_prepared_directory_with(
    prepared: &crate::PreparedDirectoryCommit,
    destination: &Path,
    verify_source: impl Fn(&Path) -> DistributionResult,
    verify_destination: impl Fn(&Path) -> DistributionResult,
    file_ops: &impl DistributionFileOps,
    managed: Option<ManagedOutput<'_>>,
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
    let destination_guard;
    let commit_guard = match &managed {
        Some(output) if output.is_root() => output.root,
        _ => {
            destination_guard = DistributionCommitGuard::acquire(parent, destination_name)?;
            &destination_guard
        }
    };
    if let Some(output) = &managed {
        output.root.ensure_held()?;
    }
    recover_stale_transactions_with_policy(
        parent,
        destination_name,
        commit_guard,
        file_ops,
        managed.as_ref(),
    )?;
    validate_commit_location(parent, destination)?;

    prepared.verify_source_contents()?;
    verify_source(prepared.staging_directory())?;

    let destination_identity = optional_directory_identity(destination)?;
    let (mut transaction, mut journal_state) = DistributionTransactionDirectory::create(
        parent,
        destination_name,
        destination_identity,
        file_ops,
    )?;
    let previous = transaction.path().join("previous");
    let journal = transaction.path().join(TRANSACTION_JOURNAL);
    let had_previous = destination_identity.is_some();
    let transaction_path = transaction.path().to_path_buf();
    let previous_path = had_previous.then(|| previous.clone());
    let mut output_changed = false;
    let mut output_was_changed = false;
    // Every fallible exit after a successful namespace mutation passes through
    // one outcome boundary. A completed rollback resets the marker; otherwise
    // callers receive the locations needed to inspect or recover the commit.
    let result = (|| {
        transaction.verify()?;
        if let Some(expected) = destination_identity {
            validate_commit_location(parent, destination)?;
            commit_guard.ensure_held()?;
            require_directory_identity(destination, expected, "original destination")?;
            if let Some(output) = &managed {
                output.validate_existing(destination)?;
                output.root.ensure_held()?;
                require_directory_identity(destination, expected, "managed destination")?;
            }
            file_ops.rename(destination, &previous)?;
            output_changed = true;
            output_was_changed = true;
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
                        &mut transaction,
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
                // The original destination and the RolledBack journal are
                // already durable. Cleanup cannot turn this into an uncertain
                // destination state, but its error must preserve the rename failure.
                output_changed = false;
                return Err(rollback_cleanup_result(
                    commit_error,
                    transaction.cleanup_now(parent, file_ops),
                ));
            }
            return Err(rollback_cleanup_result(
                commit_error,
                file_ops.sync_directory(parent),
            ));
        }
        output_changed = true;
        output_was_changed = true;
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
            .and_then(|()| {
                // Check the exact tree moved out of the way, not merely its
                // name or inode. A new foreign file is grounds for rollback.
                if let (Some(output), true) = (&managed, had_previous) {
                    output.validate_existing(&previous)?;
                }
                Ok(())
            })
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
                        &mut transaction,
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
                        &mut transaction,
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
                if let Err(cleanup) =
                    remove_installed_distribution_with(&failed, &journal_state, file_ops)
                {
                    return Err(DistributionError::CleanupFailure {
                        phase: "post-commit verification rollback",
                        primary: Box::new(verification_error),
                        cleanup: Box::new(cleanup),
                    });
                }
            } else if let Err(rollback_error) = validate_commit_location(parent, destination)
                .map_err(error_as_io)
                .and_then(|_| {
                    remove_installed_distribution_with(destination, &journal_state, file_ops)
                        .map_err(error_as_io)
                })
            {
                return Err(distribution_recovery_error(
                    &mut transaction,
                    destination,
                    error_as_io(verification_error),
                    rollback_error,
                )
                .into());
            }
            if let Err(cleanup) = transaction.cleanup_now(parent, file_ops) {
                return Err(DistributionError::CleanupFailure {
                    phase: "post-commit verification rollback",
                    primary: Box::new(verification_error),
                    cleanup: Box::new(cleanup.into()),
                });
            }
            output_changed = false;
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
    })();
    match result {
        Err(error) if output_changed => Err(interrupted_distribution(
            error,
            destination,
            transaction_path,
            previous_path,
        )),
        Err(primary) if !output_was_changed => {
            transaction.capability.take();
            let cleanup = (|| -> PackageResult {
                commit_guard.ensure_held()?;
                if optional_directory_identity(destination)? != destination_identity {
                    return Err(
                        "destination changed before the transaction could be cleaned".into(),
                    );
                }
                cleanup_unmutated_transaction(
                    parent,
                    &transaction_path,
                    journal_state.transaction_identity,
                    file_ops,
                )
            })();
            Err(cleanup_after_unmutated_failure(
                destination,
                &transaction_path,
                primary,
                cleanup,
            ))
        }
        outcome => outcome,
    }
}

#[derive(Debug)]
struct IoErrorSource(DistributionError);

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

fn error_as_io<E>(error: E) -> io::Error
where
    E: Into<DistributionError>,
{
    // Keep the typed distribution error behind the standard I/O error rather
    // than flattening it to a display string.
    io::Error::other(IoErrorSource(error.into()))
}

fn rollback_cleanup_result(commit_error: io::Error, cleanup: io::Result<()>) -> DistributionError {
    let primary = DistributionError::Io(commit_error);
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => DistributionError::CleanupFailure {
            phase: "distribution commit rollback",
            primary: Box::new(primary),
            cleanup: Box::new(cleanup.into()),
        },
    }
}

fn distribution_recovery_error(
    transaction: &mut DistributionTransactionDirectory,
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

fn cleanup_after_unmutated_failure(
    destination: &Path,
    transaction: &Path,
    primary: DistributionError,
    cleanup: PackageResult,
) -> DistributionError {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => interrupted_distribution(
            DistributionRecoveryError {
                destination: destination.to_path_buf(),
                commit_error: error_as_io(primary),
                rollback_error: error_as_io(cleanup),
                recovery_path: transaction.join("previous"),
            }
            .into(),
            destination,
            transaction.to_path_buf(),
            None,
        ),
    }
}

fn quarantine_transaction(
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

#[cfg(test)]
fn recover_stale_transactions(
    parent: &Path,
    destination_name: &str,
    commit_guard: &DistributionCommitGuard,
    file_ops: &impl DistributionFileOps,
) -> DistributionResult {
    recover_stale_transactions_with_policy(parent, destination_name, commit_guard, file_ops, None)
}

fn recover_stale_transactions_with_policy(
    parent: &Path,
    destination_name: &str,
    commit_guard: &DistributionCommitGuard,
    file_ops: &impl DistributionFileOps,
    managed: Option<&ManagedOutput<'_>>,
) -> DistributionResult {
    commit_guard.ensure_held()?;
    if commit_guard.destination() != parent.join(destination_name) {
        return Err(DistributionError::Invalid(
            "distribution recovery lock does not match its destination".to_owned(),
        ));
    }
    crate::validate_directory_path(parent)?;
    let prefix = format!(".{destination_name}.transaction-");
    let transactions =
        pending_transaction_paths(parent, &prefix)?.collect::<io::Result<Vec<_>>>()?;

    for transaction in transactions {
        recover_distribution_transaction(
            parent,
            destination_name,
            &prefix,
            &transaction,
            commit_guard,
            file_ops,
            managed,
        )?;
    }
    Ok(())
}

fn pending_transaction_paths<'a>(
    parent: &Path,
    prefix: &'a str,
) -> io::Result<impl Iterator<Item = io::Result<PathBuf>> + use<'a>> {
    Ok(fs::read_dir(parent)?.filter_map(move |entry| match entry {
        Ok(entry) => entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(prefix))
            .then(|| Ok(entry.path())),
        Err(error) => Some(Err(error)),
    }))
}

fn recover_distribution_transaction(
    parent: &Path,
    destination_name: &str,
    prefix: &str,
    transaction: &Path,
    commit_guard: &DistributionCommitGuard,
    file_ops: &impl DistributionFileOps,
    managed: Option<&ManagedOutput<'_>>,
) -> DistributionResult {
    let destination = commit_guard.destination();
    let mut previous_path = Some(transaction.join("previous"));
    let result = (|| {
        commit_guard.ensure_held()?;
        // Directory names in the output parent are untrusted. Establish owner
        // and private permissions before enumerating payloads or reading either
        // journal, including invalid-journal cleanup and quarantine paths.
        PrivateStagingDirectory::open(transaction)?.verify()?;
        let private_transaction = is_private_transaction(transaction, prefix)?;
        let has_payloads = transaction_payloads(transaction)?
            .next()
            .transpose()?
            .is_some();
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
                    prefix,
                    transaction,
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
                        prefix,
                        transaction,
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
                    prefix,
                    transaction,
                    "journal.next is not a regular file",
                    file_ops,
                );
            }
            let next_state = match read_transaction_journal(&next) {
                Ok(state) => state,
                Err(_error) if !has_payloads => {
                    fs::remove_file(&next)?;
                    file_ops.sync_directory(transaction)?;
                    remove_empty_transaction(parent, transaction, file_ops)?;
                    return Ok(());
                }
                Err(error) => {
                    return quarantine_transaction(
                        parent,
                        destination_name,
                        prefix,
                        transaction,
                        format!("journal.next is invalid: {error}"),
                        file_ops,
                    );
                }
            };
            let transaction_directory = validate_transaction_provenance(
                parent,
                destination_name,
                prefix,
                transaction,
                &next_state,
            )?;
            transaction_directory.verify()?;
            atomic_replace_file(&next, &journal)?;
            file_ops.sync_directory(transaction)?;
            file_ops.sync_directory(parent)?;
            next_metadata = None;
            next_state
        } else if !has_payloads {
            remove_empty_transaction(parent, transaction, file_ops)?;
            return Ok(());
        } else {
            return quarantine_transaction(
                parent,
                destination_name,
                prefix,
                transaction,
                "transaction journal is missing while recovery payloads remain",
                file_ops,
            );
        };

        previous_path = (journal_state.destination_identity.is_some()
            || journal_state.previous_identity.is_some())
        .then(|| transaction.join("previous"));

        let transaction_directory = validate_transaction_provenance(
            parent,
            destination_name,
            prefix,
            transaction,
            &journal_state,
        )?;

        if let Some(metadata) = next_metadata.as_ref() {
            if metadata.is_file() {
                match read_transaction_journal(&next) {
                    Ok(next_state) if next_state.sequence > journal_state.sequence => {
                        validate_transaction_provenance(
                            parent,
                            destination_name,
                            prefix,
                            transaction,
                            &next_state,
                        )?
                        .verify()?;
                        atomic_replace_file(&next, &journal)?;
                        file_ops.sync_directory(transaction)?;
                        file_ops.sync_directory(parent)?;
                        journal_state = next_state;
                    }
                    Ok(_) | Err(_) => {
                        fs::remove_file(&next)?;
                        file_ops.sync_directory(transaction)?;
                    }
                }
            } else {
                return quarantine_transaction(
                    parent,
                    destination_name,
                    prefix,
                    transaction,
                    "journal.next is not a regular file",
                    file_ops,
                );
            }
        }

        if private_transaction {
            if has_payloads
                || journal_state.state != TransactionState::Prepared
                || journal_state.previous_identity.is_some()
                || journal_state.installed_identity.is_some()
            {
                return quarantine_transaction(
                    parent,
                    destination_name,
                    prefix,
                    transaction,
                    "private transaction contains state from after publication",
                    file_ops,
                );
            }
            // Output renames are only possible after publishing the transaction
            // directory. No recovery action depends on the destination that
            // existed when this private journal was written.
            commit_guard.ensure_held()?;
            previous_path = None;
            drop(transaction_directory);
            cleanup_unmutated_transaction(
                parent,
                transaction,
                journal_state.transaction_identity,
                file_ops,
            )?;
            return Ok(());
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
                    if optional_directory_identity(destination)?.is_some() {
                        return Err(DistributionError::Invalid(format!(
                            "distribution transaction journal is inconsistent: {}",
                            transaction.display()
                        )));
                    }
                    restore_previous_distribution(
                        parent,
                        destination,
                        transaction,
                        &previous,
                        &mut journal_state,
                        file_ops,
                    )?;
                } else if let Some(expected) = journal_state.destination_identity {
                    require_directory_identity(destination, expected, "original destination")?;
                }
            }
            TransactionState::NoPrevious => {
                if optional_directory_identity(&previous)?.is_some() {
                    return Err(DistributionError::Invalid(format!(
                        "distribution transaction journal is inconsistent: {}",
                        transaction.display()
                    )));
                }
                if optional_directory_identity(destination)?.is_some()
                    && journal_state.destination_identity.is_none()
                {
                    return Err(DistributionError::Invalid(format!(
                        "no-previous transaction has an unexpected destination: {}",
                        transaction.display()
                    )));
                }
                if let Some(expected) = journal_state.destination_identity {
                    require_directory_identity(destination, expected, "original destination")?;
                }
            }
            TransactionState::InstallPending => {
                if optional_directory_identity(&previous)?.is_some() {
                    restore_previous_distribution(
                        parent,
                        destination,
                        transaction,
                        &previous,
                        &mut journal_state,
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
                remove_installed_distribution_with(destination, &journal_state, file_ops)?;
                remove_recovery_install(transaction, &journal_state, file_ops)?;
            }
            TransactionState::PreviousSaved => {
                if optional_directory_identity(&previous)?.is_some() {
                    restore_previous_distribution(
                        parent,
                        destination,
                        transaction,
                        &previous,
                        &mut journal_state,
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
                        parent,
                        destination,
                        transaction,
                        &previous,
                        &mut journal_state,
                        file_ops,
                    )?;
                } else if journal_state.state == TransactionState::RollbackPending {
                    require_original_destination(destination, &journal_state)?;
                    remove_recovery_install(transaction, &journal_state, file_ops)?;
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
                remove_installed_distribution_with(destination, &journal_state, file_ops)?;
                remove_recovery_install(transaction, &journal_state, file_ops)?;
            }
            TransactionState::RolledBack => {
                if optional_directory_identity(&previous)?.is_some() {
                    restore_previous_distribution(
                        parent,
                        destination,
                        transaction,
                        &previous,
                        &mut journal_state,
                        file_ops,
                    )?;
                } else {
                    require_original_destination(destination, &journal_state)?;
                }
                remove_recovery_install(transaction, &journal_state, file_ops)?;
            }
            TransactionState::Committed | TransactionState::CleanupPending => {
                let installed = journal_state.installed_identity.ok_or_else(|| {
                    DistributionError::Invalid(
                        "committed transaction has no installed identity".to_owned(),
                    )
                })?;
                if journal_state.state == TransactionState::Committed {
                    if optional_directory_identity(destination)?.is_none() {
                        // Committed means the installed directory is authoritative.
                        // The old distribution is cleanup payload only and must
                        // never become a recovery source after commit.
                        return quarantine_transaction(
                            parent,
                            destination_name,
                            prefix,
                            transaction,
                            "committed destination is missing; refusing to restore the previous distribution",
                            file_ops,
                        );
                    }
                    require_directory_identity(destination, installed, "installed destination")?;
                }
                if optional_directory_identity(&previous)?.is_some() {
                    let expected = journal_state.previous_identity.ok_or_else(|| {
                        DistributionError::Invalid(
                            "committed transaction has no previous identity".to_owned(),
                        )
                    })?;
                    require_directory_identity(&previous, expected, "committed backup")?;
                    if let Some(output) = managed
                        && let Err(error) = output.validate_existing(&previous)
                    {
                        // Legacy journals only attest the directory's
                        // identity, not whether its contents were managed.
                        // A partially cleaned tree is likewise unproven.
                        return quarantine_transaction(
                            parent,
                            destination_name,
                            prefix,
                            transaction,
                            error.to_string(),
                            file_ops,
                        );
                    }
                    if file_ops.remove_dir_all(&previous).is_err() {
                        // A loaded old XLL can keep the backup locked after a
                        // successful commit. Persist the cleanup-only handoff
                        // before allowing another commit to change destination;
                        // otherwise this journal's installed identity would
                        // reject every later recovery of the retained backup.
                        transaction_directory.verify()?;
                        commit_guard.ensure_held()?;
                        if journal_state.state == TransactionState::Committed {
                            write_transaction_state(
                                &journal,
                                parent,
                                &mut journal_state,
                                TransactionState::CleanupPending,
                                file_ops,
                            )?;
                        } else {
                            // A preceding attempt may have replaced the journal
                            // before its directory sync failed. Re-establish the
                            // durable handoff before permitting a new install.
                            file_ops.sync_directory(transaction)?;
                            file_ops.sync_directory(parent)?;
                        }
                        return Ok(());
                    }
                    file_ops.sync_directory(transaction)?;
                }
                remove_recovery_install(transaction, &journal_state, file_ops)?;
            }
        }
        commit_guard.ensure_held()?;
        transaction_directory.verify()?;
        drop(transaction_directory);
        file_ops.remove_dir_all(transaction)?;
        file_ops.sync_directory(parent)?;
        Ok(())
    })();
    result.map_err(|error| {
        interrupted_distribution(error, destination, transaction.to_path_buf(), previous_path)
    })
}

fn interrupted_distribution(
    source: DistributionError,
    destination: &Path,
    transaction: PathBuf,
    previous: Option<PathBuf>,
) -> DistributionError {
    match source {
        // These outcomes already preserve explicit transaction locations.
        DistributionError::Recovery(_)
        | DistributionError::Quarantined(_)
        | DistributionError::Interrupted { .. } => source,
        source => DistributionError::Interrupted {
            destination: destination.to_path_buf(),
            transaction,
            previous,
            source: Box::new(source),
        },
    }
}

fn require_original_destination(
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

fn remove_installed_distribution_with(
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

fn restore_previous_distribution(
    parent: &Path,
    destination: &Path,
    transaction: &Path,
    previous: &Path,
    journal: &mut TransactionJournal,
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
    if !matches!(
        journal.state,
        TransactionState::RollbackPending | TransactionState::RolledBack
    ) {
        // Restoring `previous` consumes its pathname. Record the recovery
        // intent first so an interrupted sync or cleanup can resume by checking
        // the original destination identity instead of requiring that backup.
        write_transaction_state(
            &transaction.join(TRANSACTION_JOURNAL),
            parent,
            journal,
            TransactionState::RollbackPending,
            file_ops,
        )?;
    }
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

fn remove_recovery_install(
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
