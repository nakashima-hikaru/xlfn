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
#[test]
fn retry_budget_returns_the_final_error_without_an_unreachable_endpoint() {
    let mut attempts = 0;
    let mut delays = Vec::new();
    let error = super::retry_path_operation(
        || {
            attempts += 1;
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("attempt {attempts}"),
            ))
        },
        |delay| delays.push(delay.as_millis()),
    )
    .unwrap_err();
    assert_eq!(attempts, 24);
    assert_eq!(error.to_string(), "attempt 24");
    assert_eq!(delays.len(), 23);
    assert_eq!(&delays[..7], &[10, 20, 40, 80, 160, 320, 500]);
    assert!(delays[7..].iter().all(|&delay| delay == 500));
}

#[test]
fn retry_stops_on_the_first_success_or_nonretryable_error() {
    let mut attempts = 0;
    let mut sleeps = 0;
    super::retry_path_operation(
        || {
            attempts += 1;
            if attempts == 3 {
                Ok(())
            } else {
                Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
            }
        },
        |_| sleeps += 1,
    )
    .unwrap();
    assert_eq!((attempts, sleeps), (3, 2));

    let mut attempts = 0;
    let error = super::retry_path_operation(
        || {
            attempts += 1;
            Err(std::io::Error::from(std::io::ErrorKind::InvalidInput))
        },
        |_| panic!("a permanent path error must not sleep"),
    )
    .unwrap_err();
    assert_eq!(attempts, 1);
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn retry_releases_the_previous_error_after_backoff_before_the_next_operation() {
    use std::sync::{Arc, Mutex};
    #[derive(Debug)]
    struct ObservedError(Arc<Mutex<Vec<&'static str>>>);
    impl std::fmt::Display for ObservedError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("retryable operation")
        }
    }
    impl std::error::Error for ObservedError {}
    impl Drop for ObservedError {
        fn drop(&mut self) {
            self.0.lock().unwrap().push("drop error");
        }
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut attempts = 0;
    super::retry_path_operation(
        || {
            events.lock().unwrap().push("attempt");
            attempts += 1;
            if attempts == 1 {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    ObservedError(Arc::clone(&events)),
                ))
            } else {
                Ok(())
            }
        },
        |_| events.lock().unwrap().push("sleep"),
    )
    .unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        ["attempt", "sleep", "drop error", "attempt"]
    );
}
