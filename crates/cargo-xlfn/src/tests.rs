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

fn prepared_test_directory(staging: &Path) -> xlfn_package::PreparedDirectoryCommit {
    xlfn_package::PreparedDirectoryCommit::prepare(staging, &["new.xll"]).unwrap()
}

fn commit_test_directory_with(
    prepared: &xlfn_package::PreparedDirectoryCommit,
    destination: &Path,
    file_ops: &impl DistributionFileOps,
) -> Result {
    commit_prepared_directory_with(
        prepared,
        destination,
        |_| Ok(()),
        |path| Ok(prepared.verify_committed_contents(path)?),
        file_ops,
    )
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

fn parse_cli_args<'a>(args: &'a [&'a str]) -> std::result::Result<Cli, usage::Error<'static, 'a>> {
    let argv: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
    Cli::parse_from(&argv)
}

#[test]
fn cargo_subcommand_name_is_removed_before_cli_parsing() {
    let args = normalize_cargo_subcommand_args(
        [
            "cargo-xlfn",
            "xlfn",
            "check",
            "--target",
            "x86_64-pc-windows-msvc",
        ]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect(),
    );
    let argv: Vec<&std::ffi::OsStr> = args.iter().map(std::ops::Deref::deref).collect();
    let parsed = Cli::parse_from_argv(&argv).unwrap();
    assert!(matches!(parsed.command, Commands::Check(_)));
}

#[test]
fn removed_native_commands_and_unknown_targets_are_rejected() {
    assert!(parse_cli_args(&["native", "inspect"]).is_err());
    assert!(parse_cli_args(&["new", "my-xll"]).is_err());
    assert!(parse_cli_args(&["package", "--target", "x86_64-pc-window-msvc"]).is_err());
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
    commit_prepared_directory(
        &prepared,
        &destination,
        |_| Ok(()),
        |path| Ok(prepared.verify_committed_contents(path)?),
    )
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

    commit_test_directory_with(&prepared, &destination, &file_ops).unwrap();

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

    commit_test_directory_with(&prepared, &destination, &file_ops).unwrap();
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
    let quarantine = error
        .downcast_ref::<DistributionRecoveryQuarantineError>()
        .and_then(|error| error.quarantine.as_ref())
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
        |_| Err(anyhow!("post-commit verification failed")),
        &file_ops,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("post-commit verification failed")
    );
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
        |_| Err(anyhow!("post-commit verification failed")),
        &file_ops,
    )
    .unwrap_err();

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
    let transaction_directory =
        xlfn_package::PrivateStagingDirectory::create(&transaction).unwrap();
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
    let transaction_directory =
        xlfn_package::PrivateStagingDirectory::create(&transaction).unwrap();
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
    let transaction_directory =
        xlfn_package::PrivateStagingDirectory::create(&transaction).unwrap();
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
    let transaction_directory =
        xlfn_package::PrivateStagingDirectory::create(&transaction).unwrap();
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
    let transaction_directory =
        xlfn_package::PrivateStagingDirectory::create(&transaction).unwrap();
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
    let quarantine = error
        .downcast_ref::<DistributionRecoveryQuarantineError>()
        .and_then(|error| error.quarantine.as_ref())
        .expect("journalless payload must be quarantined");

    assert!(!transaction.exists());
    assert_eq!(
        fs::read(quarantine.join("previous/old.xll")).unwrap(),
        b"old"
    );
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
    let transaction_directory =
        xlfn_package::PrivateStagingDirectory::create(&transaction).unwrap();
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
    let recovery = error
        .downcast_ref::<DistributionRecoveryError>()
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
fn transactional_distribution_requires_a_dedicated_output_directory() {
    assert!(validate_transactional_output_root(Path::new(".")).is_err());
    assert!(validate_transactional_output_root(Path::new("..")).is_err());
    assert!(validate_transactional_output_root(Path::new("/")).is_err());
    assert!(validate_transactional_output_root(Path::new("dist")).is_ok());
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
fn generated_distribution_basenames_are_reserved_case_insensitively() {
    assert!(is_reserved_distribution_name("calcaddin.XLL", "CalcAddin"));
    assert!(is_reserved_distribution_name(
        "BUILD-MANIFEST.JSON",
        "CalcAddin"
    ));
    assert!(!is_reserved_distribution_name(
        "CalcEngine.dll",
        "CalcAddin"
    ));
}

#[test]
fn windows_basename_validator_rejects_invalid_names() {
    assert!(validate_windows_basename("foo:bar").is_err());
    assert!(validate_windows_basename("foo*").is_err());
    assert!(validate_windows_basename("foo?").is_err());
    assert!(validate_windows_basename("name.").is_err());
    assert!(validate_windows_basename("name ").is_err());
    assert!(validate_windows_basename("CON").is_err());
    assert!(validate_windows_basename("AUX").is_err());
    assert!(validate_windows_basename("NUL").is_err());
    assert!(validate_windows_basename("COM1").is_err());
    assert!(validate_windows_basename("valid-artifact-name").is_ok());
}

#[test]
fn bundle_metadata_type_error_reports_nested_path() {
    let error = parse_bundle_metadata(serde_json::json!({
        "x86": ["foo.dll", 42]
    }))
    .unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains("[package.metadata.xlfn.bundle]"),
        "{message}"
    );
    assert!(message.contains("x86[1]"), "{message}");
}

#[test]
fn bundle_metadata_scalar_error_reports_field_path() {
    let error = parse_bundle_metadata(serde_json::json!({
        "strict-paths": "yes"
    }))
    .unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains("[package.metadata.xlfn.bundle]"),
        "{message}"
    );
    assert!(message.contains("strict-paths"), "{message}");
}

#[test]
fn bundle_metadata_path_tracking_preserves_valid_deserialization() {
    let metadata = parse_bundle_metadata(serde_json::json!({
        "x86": ["foo.dll"],
        "x64": ["bar.dll"],
        "external-imports": ["engine.dll"],
        "strict-paths": true
    }))
    .unwrap();

    assert_eq!(metadata.x86, vec!["foo.dll"]);
    assert_eq!(metadata.x64, vec!["bar.dll"]);
    assert_eq!(metadata.external_imports, vec!["engine.dll"]);
    assert!(metadata.strict_paths);
}

#[test]
fn check_args_supports_build_selection_flags() {
    let parsed = parse_cli_args(&[
        "check",
        "--profile",
        "release",
        "--features",
        "feat1,feat2",
        "--crt",
        "dynamic",
        "--locked",
    ])
    .unwrap();
    let Commands::Check(args) = parsed.command else {
        panic!("expected Check command");
    };
    let build = args.build();
    assert_eq!(build.profile.as_deref(), Some("release"));
    assert_eq!(build.normalized_features(), vec!["feat1", "feat2"]);
    assert_eq!(build.crt, Some(CrtPolicy::Dynamic));
    assert!(build.locked);
}

#[test]
fn cli_accepts_every_crt_policy() {
    for policy in ["inherit", "static", "dynamic"] {
        assert!(
            parse_cli_args(&["check", "--crt", policy]).is_ok(),
            "policy {policy} should parse"
        );
    }
    assert!(parse_cli_args(&["check", "--crt", "auto"]).is_err());
}

#[test]
fn metadata_uses_the_same_feature_and_resolution_constraints_as_build() {
    let selection = BuildSelectionArgs {
        features: vec!["feat1".to_owned(), "feat2".to_owned()],
        no_default_features: true,
        all_features: false,
        locked: true,
        frozen: false,
        offline: true,
        ..BuildSelectionArgs::default()
    };
    let mut metadata = MetadataCommand::new();
    selection.apply_to_metadata(&mut metadata);
    let command = metadata.cargo_command();
    let arguments = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(
        arguments
            .array_windows::<2>()
            .any(|[flag, value]| flag == "--features" && value == "feat1,feat2")
    );
    assert!(
        arguments
            .iter()
            .any(|argument| argument == "--no-default-features")
    );
    assert!(arguments.iter().any(|argument| argument == "--locked"));
    assert!(arguments.iter().any(|argument| argument == "--offline"));
}

#[test]
fn base_cargo_command_does_not_rewrite_rustflags() {
    let command = cargo_command();
    let arguments = command
        .get_args()
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>();
    let environment = command
        .get_envs()
        .filter_map(|(_, value)| value)
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>();
    assert!(arguments.is_empty());
    assert!(!environment.iter().any(|value| {
        let value = value.to_string_lossy();
        value.contains("crt-static") || value.contains("RUSTFLAGS")
    }));
}

#[test]
fn configure_build_sets_target_dir_and_build_dir() {
    let metadata = ProjectMetadata {
        package_name: "test-pkg".into(),
        package_version: "0.1.0".into(),
        lib_name: "test_pkg".into(),
        artifact_name: "test_pkg".into(),
        manifest_path: PathBuf::from("Cargo.toml"),
        manifest_directory: PathBuf::from("."),
        target_directory: PathBuf::from("target"),
        crt: crt::ResolvedCrtPolicy::resolve(Some(crt::CrtPolicy::Static), None),
        resolved_features: Vec::new(),
        lockfile_sha256: None,
        bundle: None,
    };
    let mut command = cargo_command();
    let temp_target = Path::new("target/temp-target");
    configure_build(
        &mut command,
        &metadata,
        "x86_64-pc-windows-msvc",
        temp_target,
    )
    .expect("configure_build succeeds");
    let args = command
        .get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(
        args.array_windows::<2>()
            .any(|[flag, val]| flag == "--target-dir" && val == "target/temp-target")
    );
    let env_build_dir = command
        .get_envs()
        .find(|(k, _)| *k == "CARGO_BUILD_BUILD_DIR")
        .and_then(|(_, v)| v.map(|s| s.to_string_lossy().into_owned()));
    assert_eq!(
        env_build_dir,
        Some(
            metadata
                .crt
                .target_directory(&metadata.target_directory)
                .join("build-cache")
                .to_string_lossy()
                .into_owned()
        )
    );
}

#[test]
fn error_as_io_retains_the_original_error_object() {
    let io_error = error_as_io(anyhow!("root cause").context("commit failed"));

    assert_eq!(io_error.to_string(), "commit failed");
    assert!(io_error.get_ref().is_some());
    let preserved = io_error
        .get_ref()
        .and_then(|error| error.downcast_ref::<IoErrorSource>())
        .expect("the anyhow error should remain attached");
    assert_eq!(preserved.0.chain().count(), 2);
}

#[test]
fn workspace_manifest_path_resolves_without_explicit_package() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/basic-xll/Cargo.toml");
    let args = ProjectArgs {
        package: None,
        manifest_path: Some(manifest),
    };
    let build = BuildSelectionArgs::default();
    let metadata = project_metadata(&args, &build).unwrap();
    assert_eq!(metadata.package_name, "basic-xlfn");
}

#[test]
fn project_metadata_reports_bundle_metadata_path() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("src")).unwrap();
    fs::write(
        directory.path().join("Cargo.toml"),
        r#"[package]
name = "bundle-diagnostic-fixture"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[package.metadata.xlfn.bundle]
x86 = ["foo.dll", 123]
"#,
    )
    .unwrap();
    fs::write(directory.path().join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();

    let args = ProjectArgs {
        package: None,
        manifest_path: Some(directory.path().join("Cargo.toml")),
    };
    let error = project_metadata(&args, &BuildSelectionArgs::default())
        .err()
        .expect("invalid bundle metadata should fail project metadata resolution");
    let message = error.to_string();

    assert!(
        message.contains("[package.metadata.xlfn.bundle]"),
        "{message}"
    );
    assert!(message.contains("x86[1]"), "{message}");
}

#[test]
fn current_directory_selects_the_containing_workspace_member() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let mut command = MetadataCommand::new();
    command.no_deps();
    command.manifest_path(workspace.join("Cargo.toml"));
    let discovery = command.exec().unwrap();
    let args = ProjectArgs::default();

    let package =
        select_discovery_package(&discovery, &args, &workspace.join("crates/xlfn/src")).unwrap();

    assert_eq!(package.name.as_str(), "xlfn");
}

#[test]
fn virtual_workspace_root_remains_ambiguous() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let mut command = MetadataCommand::new();
    command.no_deps();
    command.manifest_path(workspace.join("Cargo.toml"));
    let discovery = command.exec().unwrap();
    let args = ProjectArgs::default();

    let error = select_discovery_package(&discovery, &args, workspace).unwrap_err();

    assert!(error.to_string().contains("--package or --manifest-path"));
}
