//! Encoding, validation, and durable writes of transaction journals.

use super::{DistributionFileOps, atomic_replace_file, require_directory_identity, transaction_id};
use crate::{DirectoryIdentity, PackageResult, PrivateStagingDirectory};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::Path;

pub(super) const TRANSACTION_JOURNAL: &str = "journal";
// Schema 2 uses BLAKE3 over the serialized journal with an empty checksum.
const TRANSACTION_SCHEMA: u32 = 2;
// Journals contain only transaction metadata, never package contents. Leave
// ample room for escaped filenames and both platform identity representations.
pub(super) const MAX_TRANSACTION_JOURNAL_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum TransactionState {
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
    // Recovery validated the committed destination but could not remove the
    // backup. This durable handoff releases the destination for later commits;
    // only this transaction's identity-checked payloads may be cleaned now.
    CleanupPending,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TransactionJournal {
    pub(super) schema: u32,
    transaction_id: String,
    pub(super) destination_name: String,
    parent_identity: DirectoryIdentity,
    pub(super) transaction_identity: DirectoryIdentity,
    pub(super) destination_identity: Option<DirectoryIdentity>,
    pub(super) state: TransactionState,
    pub(super) previous_identity: Option<DirectoryIdentity>,
    pub(super) installed_identity: Option<DirectoryIdentity>,
    pub(super) sequence: u64,
    pub(super) checksum: String,
}

impl TransactionJournal {
    pub(super) fn new(
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

    pub(super) fn refresh_checksum(&mut self) -> PackageResult {
        self.checksum = journal_checksum(self)?;
        Ok(())
    }
}

/// Reads and validates a regular, non-symlink transaction journal of at most 64 KiB.
pub(super) fn read_transaction_journal(path: &Path) -> PackageResult<TransactionJournal> {
    crate::validate_path_components(path)?;
    let file = crate::open_staged_file_no_follow(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || crate::is_reparse_point(&metadata) {
        return Err(format!(
            "transaction journal is not a regular file: {}",
            path.display()
        )
        .into());
    }
    check_transaction_journal_size(path, metadata.len())?;
    read_transaction_journal_contents(path, file)
}

fn check_transaction_journal_size(path: &Path, bytes: u64) -> PackageResult {
    if bytes > MAX_TRANSACTION_JOURNAL_BYTES as u64 {
        return Err(format!(
            "distribution transaction journal exceeds the {MAX_TRANSACTION_JOURNAL_BYTES}-byte limit: {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

pub(super) fn read_transaction_journal_contents(
    path: &Path,
    reader: impl Read,
) -> PackageResult<TransactionJournal> {
    // The streaming bound also covers a file growing after metadata was read.
    // Read one extra byte to distinguish an exact-limit journal from truncation.
    let mut bytes = Vec::new();
    reader
        .take(MAX_TRANSACTION_JOURNAL_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    check_transaction_journal_size(path, bytes.len() as u64)?;
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
///
/// Rejects encoded journals larger than 64 KiB before creating `journal.next`.
pub(super) fn write_transaction_state(
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
    check_transaction_journal_size(journal, encoded.len() as u64)?;
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

/// Validates the journal's identity and destination provenance.
pub(super) fn validate_transaction_provenance(
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

fn journal_checksum(journal: &TransactionJournal) -> PackageResult<String> {
    let mut unsigned = journal.clone();
    unsigned.checksum.clear();
    let encoded = serde_json::to_vec(&unsigned)?;
    Ok(blake3::hash(&encoded).to_hex().to_string())
}
