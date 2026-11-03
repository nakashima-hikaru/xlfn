use super::*;

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;

#[derive(Debug, PartialEq)]
enum FileOperation {
    Rename { from: PathBuf, to: PathBuf },
    RemoveDirectory(PathBuf),
}

#[derive(Default)]
struct InjectedFileOps {
    failed_renames: BTreeSet<usize>,
    failed_removes: BTreeSet<usize>,
    failed_syncs: BTreeSet<usize>,
    fail_sync_after_backup: bool,
    fail_sync_after_install: bool,
    rename_count: Cell<usize>,
    remove_count: Cell<usize>,
    sync_count: Cell<usize>,
    operations: RefCell<Vec<FileOperation>>,
}

impl InjectedFileOps {
    fn failing_renames(calls: impl IntoIterator<Item = usize>) -> Self {
        Self {
            failed_renames: calls.into_iter().collect(),
            ..Self::default()
        }
    }

    fn failing_removes(calls: impl IntoIterator<Item = usize>) -> Self {
        Self {
            failed_removes: calls.into_iter().collect(),
            ..Self::default()
        }
    }

    fn failing_syncs(calls: impl IntoIterator<Item = usize>) -> Self {
        Self {
            failed_syncs: calls.into_iter().collect(),
            ..Self::default()
        }
    }
}

impl DistributionFileOps for InjectedFileOps {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let call = self.rename_count.get() + 1;
        self.rename_count.set(call);
        self.operations.borrow_mut().push(FileOperation::Rename {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        });
        if self.failed_renames.contains(&call) {
            return Err(io::Error::other(format!("injected rename failure #{call}")));
        }
        fs::rename(from, to)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        let call = self.remove_count.get() + 1;
        self.remove_count.set(call);
        self.operations
            .borrow_mut()
            .push(FileOperation::RemoveDirectory(path.to_path_buf()));
        if self.failed_removes.contains(&call) {
            return Err(io::Error::other(format!(
                "injected directory removal failure #{call}"
            )));
        }
        fs::remove_dir_all(path)
    }

    fn sync_directory(&self, path: &Path) -> io::Result<()> {
        let call = self.sync_count.get() + 1;
        self.sync_count.set(call);
        if let Some(FileOperation::Rename { to, .. }) = self.operations.borrow().last() {
            let backup = to.file_name().is_some_and(|name| name == "previous");
            let install = to.file_name().is_some_and(|name| name == "win-x64");
            let rename_succeeded = !self.failed_renames.contains(&self.rename_count.get());
            if rename_succeeded
                && ((self.fail_sync_after_backup && backup)
                    || (self.fail_sync_after_install && install))
            {
                return Err(io::Error::other(
                    "injected sync failure after namespace change",
                ));
            }
        }
        if self.failed_syncs.contains(&call) {
            return Err(io::Error::other(format!(
                "injected directory sync failure #{call}"
            )));
        }
        sync_directory(path)
    }
}

fn distribution_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("win-x64");
    let staging = directory.path().join("staging");
    fs::create_dir_all(&destination).unwrap();
    fs::create_dir_all(&staging).unwrap();
    fs::write(destination.join("old.xll"), b"old").unwrap();
    fs::write(staging.join("new.xll"), b"new").unwrap();
    (directory, destination, staging)
}

fn prepared_test_directory(staging: &Path) -> crate::PreparedDirectoryCommit {
    crate::PreparedDirectoryCommit::prepare(staging, &["new.xll"]).unwrap()
}

fn commit_test_directory_with(
    prepared: &crate::PreparedDirectoryCommit,
    destination: &Path,
    file_ops: &impl DistributionFileOps,
) -> DistributionResult {
    let _ = commit_prepared_directory_with(
        prepared,
        destination,
        |_| Ok(()),
        |path| Ok(prepared.verify_committed_contents(path)?),
        file_ops,
    )?;
    Ok(())
}

fn transaction_directories(parent: &Path) -> Vec<PathBuf> {
    fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".win-x64.transaction-"))
        })
        .collect()
}

fn write_stale_journal(
    parent: &Path,
    transaction: &Path,
    destination_name: &str,
    destination_identity: Option<DirectoryIdentity>,
    state: TransactionState,
    previous_identity: Option<DirectoryIdentity>,
    installed_identity: Option<DirectoryIdentity>,
) {
    let prefix = format!(".{destination_name}.transaction-");
    let mut journal = TransactionJournal::new(
        transaction_id(transaction, &prefix).unwrap(),
        destination_name.to_owned(),
        directory_identity(parent).unwrap(),
        directory_identity(transaction).unwrap(),
        destination_identity,
    )
    .unwrap();
    journal.previous_identity = previous_identity;
    journal.installed_identity = installed_identity;
    write_transaction_state(
        &transaction.join(TRANSACTION_JOURNAL),
        parent,
        &mut journal,
        state,
        &SystemDistributionFileOps,
    )
    .unwrap();
}

#[test]
fn distribution_commit_preserves_unrelated_previous_directory() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("win-x64");
    let unrelated = directory.path().join("win-x64.previous");
    let staging = directory.path().join("staging");
    fs::create_dir_all(&destination).unwrap();
    fs::create_dir_all(&unrelated).unwrap();
    fs::create_dir_all(&staging).unwrap();
    fs::write(destination.join("old.xll"), b"old").unwrap();
    fs::write(unrelated.join("sentinel.txt"), b"keep").unwrap();
    fs::write(staging.join("new.xll"), b"new").unwrap();

    let prepared = prepared_test_directory(&staging);
    crate::PreparedDistribution::new(prepared)
        .commit(&destination)
        .unwrap();

    assert_eq!(fs::read(destination.join("new.xll")).unwrap(), b"new");
    assert!(!destination.join("old.xll").exists());
    assert_eq!(fs::read(unrelated.join("sentinel.txt")).unwrap(), b"keep");
}

#[test]
fn distribution_commit_removes_backup_only_after_installing_staging() {
    let (directory, destination, staging) = distribution_fixture();
    let file_ops = InjectedFileOps::default();
    let prepared = prepared_test_directory(&staging);

    let _outcome = crate::PreparedDistribution::new(prepared)
        .commit_with(&destination, &file_ops)
        .unwrap();

    assert_eq!(fs::read(destination.join("new.xll")).unwrap(), b"new");
    assert!(!destination.join("old.xll").exists());
    assert!(transaction_directories(directory.path()).is_empty());
    let operations = file_ops.operations.borrow();
    assert_eq!(operations.len(), 4);
    let FileOperation::Rename {
        from: old_destination,
        to: backup,
    } = &operations[0]
    else {
        panic!("first operation must preserve the old distribution");
    };
    assert_eq!(old_destination, &destination);
    let FileOperation::Rename {
        from: committed_staging,
        to: committed_destination,
    } = &operations[1]
    else {
        panic!("second operation must commit staging");
    };
    assert_eq!(committed_staging, &staging);
    assert_eq!(committed_destination, &destination);
    assert_eq!(
        operations[2],
        FileOperation::RemoveDirectory(backup.clone())
    );
    assert!(matches!(
        &operations[3],
        FileOperation::RemoveDirectory(path)
            if path.file_name().is_some_and(|name| name
                .to_string_lossy()
                .starts_with(".win-x64.transaction-"))
    ));
}

#[test]
fn committed_cleanup_failure_never_restores_previous_distribution() {
    let (directory, destination, staging) = distribution_fixture();
    let file_ops = InjectedFileOps::failing_removes([1]);
    let prepared = prepared_test_directory(&staging);

    let outcome = crate::PreparedDistribution::new(prepared)
        .commit_with(&destination, &file_ops)
        .unwrap();
    assert!(matches!(
        outcome.cleanup(),
        crate::CleanupOutcome::BackupRetained { .. }
    ));
    assert_eq!(fs::read(destination.join("new.xll")).unwrap(), b"new");

    let transaction = transaction_directories(directory.path())
        .into_iter()
        .next()
        .expect("failed backup cleanup must leave the committed journal");
    fs::remove_dir_all(&destination).unwrap();

    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    let error = recover_stale_transactions(
        directory.path(),
        "win-x64",
        &guard,
        &SystemDistributionFileOps,
    )
    .unwrap_err();
    let DistributionError::Quarantined(error) = error else {
        panic!("a missing committed destination must be quarantined");
    };
    let quarantine = error
        .quarantine
        .as_ref()
        .expect("a missing committed destination must be quarantined");

    assert!(!destination.exists());
    assert!(!transaction.exists());
    assert_eq!(
        fs::read(quarantine.join("previous/old.xll")).unwrap(),
        b"old"
    );
}

#[test]
fn distribution_commit_failure_rolls_previous_distribution_back() {
    let (directory, destination, staging) = distribution_fixture();
    let file_ops = InjectedFileOps::failing_renames([2]);
    let prepared = prepared_test_directory(&staging);

    let error = commit_test_directory_with(&prepared, &destination, &file_ops).unwrap_err();

    assert!(error.to_string().contains("injected rename failure #2"));
    assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");
    assert_eq!(fs::read(staging.join("new.xll")).unwrap(), b"new");
    assert!(transaction_directories(directory.path()).is_empty());
    assert_eq!(file_ops.rename_count.get(), 3);
    assert!(
        file_ops
            .operations
            .borrow()
            .iter()
            .any(|operation| matches!(
                operation,
                FileOperation::RemoveDirectory(path)
                    if path.file_name().is_some_and(|name| name
                        .to_string_lossy()
                        .starts_with(".win-x64.transaction-"))
            ))
    );
}

#[test]
fn post_commit_verification_failure_restores_previous_distribution() {
    let (directory, destination, staging) = distribution_fixture();
    let file_ops = InjectedFileOps::default();
    let prepared = prepared_test_directory(&staging);

    let error = commit_prepared_directory_with(
        &prepared,
        &destination,
        |_| Ok(()),
        |_| {
            Err(DistributionError::Invalid(
                "post-commit verification failed".to_owned(),
            ))
        },
        &file_ops,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("post-commit verification failed")
    );
    assert!(matches!(&error, DistributionError::Invalid(_)));
    assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");
    assert!(!destination.join("new.xll").exists());
    assert!(!staging.exists());
    assert!(transaction_directories(directory.path()).is_empty());
}

#[test]
fn post_commit_verification_failure_without_previous_removes_install() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("win-x64");
    let staging = directory.path().join("staging");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("new.xll"), b"new").unwrap();
    let file_ops = InjectedFileOps::default();
    let prepared = prepared_test_directory(&staging);

    let error = commit_prepared_directory_with(
        &prepared,
        &destination,
        |_| Ok(()),
        |_| {
            Err(DistributionError::Invalid(
                "post-commit verification failed".to_owned(),
            ))
        },
        &file_ops,
    )
    .unwrap_err();

    assert!(matches!(&error, DistributionError::Invalid(_)));

    assert!(
        error
            .to_string()
            .contains("post-commit verification failed")
    );
    assert!(!destination.exists());
    assert!(!staging.exists());
    assert!(transaction_directories(directory.path()).is_empty());
    assert!(matches!(
        file_ops.operations.borrow().as_slice(),
        [
            FileOperation::Rename { .. },
            FileOperation::RemoveDirectory(path),
            FileOperation::RemoveDirectory(transaction)
        ] if path == &destination
            && transaction
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".win-x64.transaction-"))
    ));
}

#[test]
fn stale_distribution_transaction_restores_previous_distribution() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("win-x64");
    let transaction = directory.path().join(".win-x64.transaction-stale");
    let transaction_directory = crate::PrivateStagingDirectory::create(&transaction).unwrap();
    let previous = transaction.join("previous");
    fs::create_dir_all(&previous).unwrap();
    fs::write(previous.join("old.xll"), b"old").unwrap();
    let previous_identity = directory_identity(&previous).unwrap();
    write_stale_journal(
        directory.path(),
        &transaction,
        "win-x64",
        Some(previous_identity),
        TransactionState::PreviousSaved,
        Some(previous_identity),
        None,
    );
    drop(transaction_directory);

    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    recover_stale_transactions(
        directory.path(),
        "win-x64",
        &guard,
        &SystemDistributionFileOps,
    )
    .unwrap();

    assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");
    assert!(!transaction.exists());
}

#[test]
fn stale_prepared_transaction_restores_backup_after_first_rename() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("win-x64");
    let transaction = directory.path().join(".win-x64.transaction-before-journal");
    let transaction_directory = crate::PrivateStagingDirectory::create(&transaction).unwrap();
    let previous = transaction.join("previous");
    fs::create_dir_all(&previous).unwrap();
    fs::write(previous.join("old.xll"), b"old").unwrap();
    let previous_identity = directory_identity(&previous).unwrap();
    write_stale_journal(
        directory.path(),
        &transaction,
        "win-x64",
        Some(previous_identity),
        TransactionState::Prepared,
        None,
        None,
    );
    drop(transaction_directory);

    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    recover_stale_transactions(
        directory.path(),
        "win-x64",
        &guard,
        &SystemDistributionFileOps,
    )
    .unwrap();

    assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");
    assert!(!transaction.exists());
}

#[test]
fn stale_empty_transaction_without_journal_is_removed() {
    let directory = tempfile::tempdir().unwrap();
    let transaction = directory.path().join(".win-x64.transaction-private-empty");
    let transaction_directory = crate::PrivateStagingDirectory::create(&transaction).unwrap();
    drop(transaction_directory);

    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    recover_stale_transactions(
        directory.path(),
        "win-x64",
        &guard,
        &SystemDistributionFileOps,
    )
    .unwrap();

    assert!(!transaction.exists());
}

#[test]
fn stale_journal_next_is_promoted_before_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("win-x64");
    let transaction = directory.path().join(".win-x64.transaction-next");
    let transaction_directory = crate::PrivateStagingDirectory::create(&transaction).unwrap();
    let previous = transaction.join("previous");
    fs::create_dir_all(&previous).unwrap();
    fs::write(previous.join("old.xll"), b"old").unwrap();
    let previous_identity = directory_identity(&previous).unwrap();
    let prefix = ".win-x64.transaction-";
    let mut journal = TransactionJournal::new(
        transaction_id(&transaction, prefix).unwrap(),
        "win-x64".to_owned(),
        directory_identity(directory.path()).unwrap(),
        directory_identity(&transaction).unwrap(),
        Some(previous_identity),
    )
    .unwrap();
    journal.previous_identity = Some(previous_identity);
    journal.sequence = 1;
    journal.refresh_checksum().unwrap();
    fs::write(
        transaction.join("journal.next"),
        serde_json::to_vec_pretty(&journal).unwrap(),
    )
    .unwrap();
    drop(transaction_directory);

    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    recover_stale_transactions(
        directory.path(),
        "win-x64",
        &guard,
        &SystemDistributionFileOps,
    )
    .unwrap();

    assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");
    assert!(!transaction.exists());
}

#[test]
fn journalless_transaction_with_payload_is_quarantined() {
    let directory = tempfile::tempdir().unwrap();
    let transaction = directory.path().join(".win-x64.transaction-orphan");
    let transaction_directory = crate::PrivateStagingDirectory::create(&transaction).unwrap();
    let previous = transaction.join("previous");
    fs::create_dir_all(&previous).unwrap();
    fs::write(previous.join("old.xll"), b"old").unwrap();
    drop(transaction_directory);

    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    let error = recover_stale_transactions(
        directory.path(),
        "win-x64",
        &guard,
        &SystemDistributionFileOps,
    )
    .unwrap_err();
    let DistributionError::Quarantined(error) = error else {
        panic!("journalless payload must be quarantined");
    };
    let quarantine = error
        .quarantine
        .as_ref()
        .expect("journalless payload must be quarantined");

    assert!(!transaction.exists());
    assert_eq!(
        fs::read(quarantine.join("previous/old.xll")).unwrap(),
        b"old"
    );
}

#[test]
fn corrupt_or_old_schema_journal_preserves_destination_and_backup() {
    for old_schema in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("win-x64");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("current.xll"), b"current").unwrap();
        let transaction = directory
            .path()
            .join(".win-x64.transaction-invalid-journal");
        let transaction_directory = crate::PrivateStagingDirectory::create(&transaction).unwrap();
        let previous = transaction.join("previous");
        fs::create_dir(&previous).unwrap();
        fs::write(previous.join("old.xll"), b"old").unwrap();
        write_stale_journal(
            directory.path(),
            &transaction,
            "win-x64",
            Some(directory_identity(&destination).unwrap()),
            TransactionState::Prepared,
            Some(directory_identity(&previous).unwrap()),
            None,
        );
        let journal_path = transaction.join(TRANSACTION_JOURNAL);
        let mut journal = read_transaction_journal(&journal_path).unwrap();
        if old_schema {
            journal.schema = 1;
            journal.refresh_checksum().unwrap();
        } else {
            // Still valid JSON, but its state no longer matches the checksum.
            journal.state = TransactionState::Committed;
        }
        fs::write(&journal_path, serde_json::to_vec_pretty(&journal).unwrap()).unwrap();
        drop(transaction_directory);

        let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
        let file_ops = InjectedFileOps::default();
        let error =
            recover_stale_transactions(directory.path(), "win-x64", &guard, &file_ops).unwrap_err();
        let DistributionError::Quarantined(error) = error else {
            panic!("invalid journal must be quarantined");
        };
        let quarantine = error.quarantine.as_ref().unwrap();
        assert_eq!(
            fs::read(destination.join("current.xll")).unwrap(),
            b"current"
        );
        assert_eq!(
            fs::read(quarantine.join("previous/old.xll")).unwrap(),
            b"old"
        );
        assert_eq!(
            file_ops.operations.borrow().as_slice(),
            &[FileOperation::Rename {
                from: transaction,
                to: quarantine.clone(),
            }]
        );
    }
}

#[cfg(unix)]
#[test]
fn distribution_lock_replacement_is_detected() {
    let directory = tempfile::tempdir().unwrap();
    let lock_path = directory.path().join(".win-x64.lock");
    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    fs::remove_file(&lock_path).unwrap();
    fs::write(&lock_path, b"replacement").unwrap();

    assert!(guard.ensure_held().is_err());
}

#[test]
fn directory_sync_failure_leaves_a_recoverable_install() {
    let (directory, destination, staging) = distribution_fixture();
    let file_ops = InjectedFileOps::failing_syncs([10]);
    let prepared = prepared_test_directory(&staging);

    let error = commit_test_directory_with(&prepared, &destination, &file_ops).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected directory sync failure #10")
    );
    assert!(transaction_directories(directory.path()).len() == 1);

    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    recover_stale_transactions(
        directory.path(),
        "win-x64",
        &guard,
        &SystemDistributionFileOps,
    )
    .unwrap();

    assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");
    assert!(!destination.join("new.xll").exists());
    assert!(transaction_directories(directory.path()).is_empty());
}

#[test]
fn backup_sync_failure_reports_interrupted_destination_and_recovery_paths() {
    let (directory, destination, staging) = distribution_fixture();
    let prepared = prepared_test_directory(&staging);
    let file_ops = InjectedFileOps {
        fail_sync_after_backup: true,
        ..InjectedFileOps::default()
    };

    let error = crate::PreparedDistribution::new(prepared)
        .commit_with(&destination, &file_ops)
        .unwrap_err();
    let DistributionError::Interrupted {
        destination: interrupted_destination,
        transaction,
        previous,
        source,
    } = error
    else {
        panic!("a failure after moving the destination must expose recovery locations");
    };
    assert_eq!(interrupted_destination, destination);
    assert!(matches!(*source, DistributionError::Io(_)));
    assert!(!destination.exists());
    let previous = previous.unwrap();
    assert_eq!(previous, transaction.join("previous"));
    assert_eq!(fs::read(previous.join("old.xll")).unwrap(), b"old");
    assert!(transaction.join(TRANSACTION_JOURNAL).is_file());

    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    recover_stale_transactions(
        directory.path(),
        "win-x64",
        &guard,
        &SystemDistributionFileOps,
    )
    .unwrap();
    assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");
    assert!(transaction_directories(directory.path()).is_empty());
}

#[test]
fn source_rejection_after_backup_reports_interruption() {
    let (directory, destination, staging) = distribution_fixture();
    let prepared = prepared_test_directory(&staging);
    let source_checks = Cell::new(0);
    let error = commit_prepared_directory_with(
        &prepared,
        &destination,
        |_| {
            source_checks.set(source_checks.get() + 1);
            if source_checks.get() == 2 {
                Err(DistributionError::Invalid(
                    "source verification rejected".to_owned(),
                ))
            } else {
                Ok(())
            }
        },
        |path| Ok(prepared.verify_committed_contents(path)?),
        &SystemDistributionFileOps,
    )
    .unwrap_err();
    let DistributionError::Interrupted {
        previous, source, ..
    } = error
    else {
        panic!("a rejected source after moving the destination must report interruption");
    };
    assert!(matches!(*source, DistributionError::Invalid(_)));
    assert!(!destination.exists());
    assert_eq!(fs::read(previous.unwrap().join("old.xll")).unwrap(), b"old");

    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    recover_stale_transactions(
        directory.path(),
        "win-x64",
        &guard,
        &SystemDistributionFileOps,
    )
    .unwrap();
    assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");
}

#[test]
fn first_install_sync_failure_reports_interruption_without_previous_package() {
    let (directory, destination, staging) = distribution_fixture();
    fs::remove_dir_all(&destination).unwrap();
    let prepared = prepared_test_directory(&staging);
    let file_ops = InjectedFileOps {
        fail_sync_after_install: true,
        ..InjectedFileOps::default()
    };
    let error = crate::PreparedDistribution::new(prepared)
        .commit_with(&destination, &file_ops)
        .unwrap_err();
    let DistributionError::Interrupted {
        transaction,
        previous,
        ..
    } = error
    else {
        panic!("a failure after installing a destination must report interruption");
    };
    assert!(previous.is_none());
    assert_eq!(fs::read(destination.join("new.xll")).unwrap(), b"new");
    assert!(transaction.join(TRANSACTION_JOURNAL).is_file());

    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    recover_stale_transactions(
        directory.path(),
        "win-x64",
        &guard,
        &SystemDistributionFileOps,
    )
    .unwrap();
    assert!(!destination.exists());
    assert!(transaction_directories(directory.path()).is_empty());
}

#[test]
fn failed_backup_rename_leaves_destination_and_plain_error() {
    let (_directory, destination, staging) = distribution_fixture();
    let prepared = prepared_test_directory(&staging);
    let error = crate::PreparedDistribution::new(prepared)
        .commit_with(&destination, &InjectedFileOps::failing_renames([1]))
        .unwrap_err();
    assert!(matches!(error, DistributionError::Io(_)));
    assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");
}

#[test]
fn initial_journal_sync_failure_leaves_only_a_recoverable_private_transaction() {
    let (directory, destination, staging) = distribution_fixture();
    let file_ops = InjectedFileOps::failing_syncs([1]);
    let prepared = prepared_test_directory(&staging);

    let error = commit_test_directory_with(&prepared, &destination, &file_ops).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected directory sync failure #1")
    );
    assert_eq!(transaction_directories(directory.path()).len(), 1);
    assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");

    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    recover_stale_transactions(
        directory.path(),
        "win-x64",
        &guard,
        &SystemDistributionFileOps,
    )
    .unwrap();

    assert!(transaction_directories(directory.path()).is_empty());
}

#[test]
fn stale_rollback_transaction_preserves_already_restored_distribution() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("win-x64");
    let transaction = directory.path().join(".win-x64.transaction-rollback");
    let transaction_directory = crate::PrivateStagingDirectory::create(&transaction).unwrap();
    let recovery_install = transaction.join("recovery-install");
    fs::create_dir_all(&destination).unwrap();
    fs::create_dir_all(&recovery_install).unwrap();
    fs::write(destination.join("old.xll"), b"old").unwrap();
    fs::write(recovery_install.join("new.xll"), b"new").unwrap();
    let destination_identity = directory_identity(&destination).unwrap();
    let installed_identity = directory_identity(&recovery_install).unwrap();
    write_stale_journal(
        directory.path(),
        &transaction,
        "win-x64",
        Some(destination_identity),
        TransactionState::RollbackPending,
        None,
        Some(installed_identity),
    );
    drop(transaction_directory);

    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    recover_stale_transactions(
        directory.path(),
        "win-x64",
        &guard,
        &SystemDistributionFileOps,
    )
    .unwrap();

    assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");
    assert!(!transaction.exists());
}

#[test]
fn distribution_preserves_recovery_path_when_commit_and_rollback_fail() {
    let (directory, destination, staging) = distribution_fixture();
    let file_ops = InjectedFileOps::failing_renames([2, 3]);
    let prepared = prepared_test_directory(&staging);

    let error = commit_test_directory_with(&prepared, &destination, &file_ops).unwrap_err();
    let recovery = match &error {
        DistributionError::Recovery(error) => Some(error),
        _ => None,
    }
    .expect("commit and rollback failure must expose the recovery path");

    assert!(error.to_string().contains("injected rename failure #2"));
    assert!(error.to_string().contains("injected rename failure #3"));
    assert!(
        error
            .to_string()
            .contains(&recovery.recovery_path.display().to_string())
    );
    assert_eq!(
        fs::read(recovery.recovery_path.join("old.xll")).unwrap(),
        b"old"
    );
    assert!(!destination.exists());
    assert_eq!(fs::read(staging.join("new.xll")).unwrap(), b"new");
    assert_eq!(
        transaction_directories(directory.path()),
        vec![recovery.recovery_path.parent().unwrap().to_path_buf()]
    );
    assert_eq!(file_ops.rename_count.get(), 3);
    assert!(
        !file_ops
            .operations
            .borrow()
            .iter()
            .any(|operation| matches!(operation, FileOperation::RemoveDirectory(_)))
    );
}

#[test]
fn rollback_sync_failure_keeps_restored_destination_recoverable() {
    let (directory, destination, staging) = distribution_fixture();
    let mut file_ops = InjectedFileOps::failing_renames([2]);
    file_ops.fail_sync_after_install = true;
    let prepared = prepared_test_directory(&staging);

    let error = commit_test_directory_with(&prepared, &destination, &file_ops).unwrap_err();
    let recovery = match &error {
        DistributionError::Recovery(error) => Some(error),
        _ => None,
    }
    .expect("a failed rollback sync must retain its recovery context");
    assert_eq!(recovery.destination, destination);
    assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");
    assert_eq!(fs::read(staging.join("new.xll")).unwrap(), b"new");
    assert!(!recovery.recovery_path.exists());
    let transaction = recovery.recovery_path.parent().unwrap();
    assert_eq!(
        read_transaction_journal(&transaction.join(TRANSACTION_JOURNAL))
            .unwrap()
            .state,
        TransactionState::RollbackPending
    );

    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    recover_stale_transactions(
        directory.path(),
        "win-x64",
        &guard,
        &SystemDistributionFileOps,
    )
    .unwrap();
    assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");
    assert!(transaction_directories(directory.path()).is_empty());
}

#[test]
fn output_destination_rejects_existing_files() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("win-x64");
    fs::write(&destination, b"do not replace").unwrap();

    let error = validate_output_destination(&destination).unwrap_err();
    assert!(error.to_string().contains("must be a directory"));

    let staging = directory.path().join("staging");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("new.xll"), b"new").unwrap();
    let file_ops = InjectedFileOps::default();
    let prepared = prepared_test_directory(&staging);
    assert!(commit_test_directory_with(&prepared, &destination, &file_ops).is_err());
    assert_eq!(fs::read(&destination).unwrap(), b"do not replace");
    assert!(file_ops.operations.borrow().is_empty());
}

#[cfg(unix)]
#[test]
fn output_destination_rejects_existing_symlinks() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let real_destination = directory.path().join("real");
    let destination = directory.path().join("win-x64");
    fs::create_dir(&real_destination).unwrap();
    symlink(&real_destination, &destination).unwrap();

    let error = validate_output_destination(&destination).unwrap_err();
    assert!(error.to_string().contains("must not be a symlink"));
}

#[cfg(unix)]
#[test]
fn output_destination_rejects_symlinked_ancestors() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let real_parent = directory.path().join("real-parent");
    let linked_parent = directory.path().join("linked-parent");
    fs::create_dir(&real_parent).unwrap();
    symlink(&real_parent, &linked_parent).unwrap();
    let destination = linked_parent.join("nested").join("win-x64");

    let error = validate_output_destination(&destination).unwrap_err();
    assert!(error.to_string().contains("must not be a symlink"));
}

#[test]
fn error_as_io_retains_the_original_error_object() {
    let io_error = error_as_io(DistributionError::Invalid("commit failed".to_owned()));

    assert_eq!(
        io_error.to_string(),
        "invalid distribution transaction: commit failed"
    );
    assert!(io_error.get_ref().is_some());
    let preserved = io_error
        .get_ref()
        .and_then(|error| error.downcast_ref::<IoErrorSource>())
        .expect("the typed distribution error should remain attached");
    assert!(matches!(
        &preserved.0,
        DistributionError::Invalid(message) if message == "commit failed"
    ));
}

#[test]
fn stale_recovery_sync_failure_reports_interruption_and_can_be_retried() {
    let (directory, destination, staging) = distribution_fixture();
    let prepared = prepared_test_directory(&staging);
    let interrupted_install = crate::PreparedDistribution::new(prepared)
        .commit_with(
            &destination,
            &InjectedFileOps {
                fail_sync_after_install: true,
                ..InjectedFileOps::default()
            },
        )
        .unwrap_err();
    let DistributionError::Interrupted { transaction, .. } = interrupted_install else {
        panic!("the installed destination must leave a recoverable transaction");
    };
    assert_eq!(fs::read(destination.join("new.xll")).unwrap(), b"new");

    let guard = DistributionCommitGuard::acquire(directory.path(), "win-x64").unwrap();
    let file_ops = InjectedFileOps::failing_syncs([1]);
    let recovery_error =
        recover_stale_transactions(directory.path(), "win-x64", &guard, &file_ops).unwrap_err();
    let DistributionError::Interrupted {
        destination: interrupted_destination,
        transaction: interrupted_transaction,
        previous,
        source,
    } = recovery_error
    else {
        panic!("recovery must report its transaction after moving the destination");
    };
    assert_eq!(interrupted_destination, destination);
    assert_eq!(interrupted_transaction, transaction);
    assert!(matches!(*source, DistributionError::Io(_)));
    assert!(!destination.exists());
    assert_eq!(previous.as_ref(), Some(&transaction.join("previous")));
    assert_eq!(fs::read(previous.unwrap().join("old.xll")).unwrap(), b"old");
    assert_eq!(
        fs::read(transaction.join("recovery-install/new.xll")).unwrap(),
        b"new"
    );

    recover_stale_transactions(
        directory.path(),
        "win-x64",
        &guard,
        &SystemDistributionFileOps,
    )
    .unwrap();
    assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");
    assert!(!destination.join("new.xll").exists());
    assert!(transaction_directories(directory.path()).is_empty());
}
