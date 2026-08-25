#![allow(
    unsafe_code,
    reason = "Distribution durability uses intentional OS file-system boundaries"
)]

//! Durable state vocabulary for distribution commits.
//!
//! The command-line layer drives the transaction, but the package layer owns
//! the on-disk journal schema and its checksum. Keeping these types here makes
//! recovery state independent from CLI orchestration.

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
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
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

        move_file_ex_with_retry(from, to, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)
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
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
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
fn move_file_ex_with_retry(
    from: &Path,
    to: &Path,
    flags: crate::win32::MOVE_FILE_FLAGS,
) -> io::Result<()> {
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
