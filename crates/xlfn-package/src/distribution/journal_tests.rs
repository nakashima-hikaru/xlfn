use super::*;

fn journal_fixture(parent: &Path, transaction: &Path) -> TransactionJournal {
    TransactionJournal::new(
        "test".to_owned(),
        "dist".to_owned(),
        crate::directory_identity(parent).unwrap(),
        crate::directory_identity(transaction).unwrap(),
        None,
    )
    .unwrap()
}

#[test]
fn journal_accepts_exact_size_limit_and_rejects_one_more_byte() {
    let parent = tempfile::tempdir().unwrap();
    let transaction = PrivateStagingDirectory::create(&parent.path().join("transaction")).unwrap();
    let path = transaction.path().join(TRANSACTION_JOURNAL);
    let mut journal = journal_fixture(parent.path(), transaction.path());
    write_transaction_state(
        &path,
        parent.path(),
        &mut journal,
        TransactionState::Prepared,
        &SystemDistributionFileOps,
    )
    .unwrap();
    let mut bytes = fs::read(&path).unwrap();
    bytes.resize(MAX_TRANSACTION_JOURNAL_BYTES, b' ');
    fs::write(&path, &bytes).unwrap();
    let decoded = read_transaction_journal(&path).unwrap();
    assert_eq!(decoded.checksum, journal.checksum);
    assert_eq!(decoded.sequence, 1);

    bytes.push(b' ');
    fs::write(&path, bytes).unwrap();
    let error = read_transaction_journal(&path).unwrap_err();
    assert!(error.to_string().contains("byte limit"), "{error}");
}

#[test]
fn journal_reader_bounds_consumption_even_without_a_reliable_file_length() {
    // Simulate a file that grows beyond its previously observed length. The
    // reader must reject it without draining the remaining input into memory.
    let mut reader = io::repeat(b' ').take(32 * 1024 * 1024);
    let initial = reader.limit();
    let error = read_transaction_journal_contents(Path::new("journal"), &mut reader).unwrap_err();
    assert!(error.to_string().contains("byte limit"), "{error}");
    assert_eq!(
        initial - reader.limit(),
        MAX_TRANSACTION_JOURNAL_BYTES as u64 + 1
    );
}

#[test]
fn journal_writer_preserves_committed_state_when_next_state_exceeds_limit() {
    let parent = tempfile::tempdir().unwrap();
    let transaction = PrivateStagingDirectory::create(&parent.path().join("transaction")).unwrap();
    let path = transaction.path().join(TRANSACTION_JOURNAL);
    let mut journal = journal_fixture(parent.path(), transaction.path());
    write_transaction_state(
        &path,
        parent.path(),
        &mut journal,
        TransactionState::Prepared,
        &SystemDistributionFileOps,
    )
    .unwrap();
    let committed = fs::read(&path).unwrap();
    journal.destination_name = "x".repeat(MAX_TRANSACTION_JOURNAL_BYTES);
    let error = write_transaction_state(
        &path,
        parent.path(),
        &mut journal,
        TransactionState::Committed,
        &SystemDistributionFileOps,
    )
    .unwrap_err();
    assert!(error.to_string().contains("byte limit"), "{error}");
    assert_eq!(fs::read(path).unwrap(), committed);
    assert!(!transaction.path().join("journal.next").exists());
}

#[cfg(unix)]
#[test]
fn recovery_rejects_nonprivate_directories_before_reading_control_files() {
    use std::os::unix::fs::PermissionsExt;

    for control_file in [TRANSACTION_JOURNAL, "journal.next"] {
        let parent = tempfile::tempdir().unwrap();
        let transaction = parent.path().join(".dist.transaction-untrusted");
        drop(PrivateStagingDirectory::create(&transaction).unwrap());
        fs::set_permissions(&transaction, std::fs::Permissions::from_mode(0o777)).unwrap();
        let journal = transaction.join(control_file);
        std::fs::File::create(&journal)
            .unwrap()
            .set_len(32 * 1024 * 1024)
            .unwrap();
        let destination = parent.path().join("dist");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("existing"), b"preserve").unwrap();

        let guard = DistributionCommitGuard::acquire(parent.path(), "dist").unwrap();
        let error =
            recover_stale_transactions(parent.path(), "dist", &guard, &SystemDistributionFileOps)
                .unwrap_err();
        let DistributionError::Interrupted { source, .. } = error else {
            panic!("untrusted directory must be rejected without quarantine");
        };
        assert!(
            source.to_string().contains("accessible by other users"),
            "{source}"
        );
        assert!(transaction.exists());
        assert_eq!(fs::metadata(journal).unwrap().len(), 32 * 1024 * 1024);
        assert_eq!(fs::read(destination.join("existing")).unwrap(), b"preserve");
    }
}

#[test]
fn oversized_recovery_journals_preserve_destination_and_backup() {
    for control_file in [TRANSACTION_JOURNAL, "journal.next"] {
        let parent = tempfile::tempdir().unwrap();
        let transaction = parent.path().join(".dist.transaction-oversized");
        drop(PrivateStagingDirectory::create(&transaction).unwrap());
        let previous = transaction.join("previous");
        fs::create_dir(&previous).unwrap();
        fs::write(previous.join("backup"), b"old").unwrap();
        std::fs::File::create(transaction.join(control_file))
            .unwrap()
            .set_len(32 * 1024 * 1024)
            .unwrap();
        let destination = parent.path().join("dist");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("existing"), b"new").unwrap();

        let guard = DistributionCommitGuard::acquire(parent.path(), "dist").unwrap();
        let error =
            recover_stale_transactions(parent.path(), "dist", &guard, &SystemDistributionFileOps)
                .unwrap_err();
        let DistributionError::Quarantined(error) = error else {
            panic!("oversized journal with a backup must be quarantined");
        };
        assert!(error.reason.contains("byte limit"), "{error}");
        let quarantine = error.quarantine.unwrap();
        assert_eq!(
            fs::read(quarantine.join("previous/backup")).unwrap(),
            b"old"
        );
        assert_eq!(fs::read(destination.join("existing")).unwrap(), b"new");
    }
}

#[cfg(unix)]
#[test]
fn journal_reader_rejects_symlinks() {
    let parent = tempfile::tempdir().unwrap();
    let source = parent.path().join("source");
    fs::write(&source, b"{}").unwrap();
    let path = parent.path().join(TRANSACTION_JOURNAL);
    std::os::unix::fs::symlink(source, &path).unwrap();
    let error = read_transaction_journal(&path).unwrap_err();
    assert!(error.to_string().contains("symlink"), "{error}");
}
