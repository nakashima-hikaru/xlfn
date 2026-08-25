use super::*;

#[derive(Debug)]
pub(crate) struct DistributionRecoveryError {
    pub(crate) destination: PathBuf,
    pub(crate) commit_error: io::Error,
    pub(crate) rollback_error: io::Error,
    pub(crate) recovery_path: PathBuf,
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
pub(crate) struct DistributionRecoveryQuarantineError {
    pub(crate) transaction: PathBuf,
    pub(crate) quarantine: Option<PathBuf>,
    pub(crate) reason: String,
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

pub(crate) static TRANSACTION_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) struct DistributionTransactionDirectory {
    pub(crate) path: PathBuf,
    pub(crate) capability: Option<xlfn_package::PrivateStagingDirectory>,
}

impl DistributionTransactionDirectory {
    pub(crate) fn create(
        parent: &Path,
        destination_name: &str,
        destination_identity: Option<DirectoryIdentity>,
        file_ops: &impl DistributionFileOps,
    ) -> Result<(Self, TransactionJournal)> {
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

            let capability = match xlfn_package::PrivateStagingDirectory::create(&private_path) {
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
            let capability = xlfn_package::PrivateStagingDirectory::open(&final_path)?;
            return Ok((
                Self {
                    path: final_path,
                    capability: Some(capability),
                },
                journal_state,
            ));
        }
        bail!("failed to allocate a unique distribution transaction directory")
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn verify(&self) -> Result<()> {
        self.capability
            .as_ref()
            .context("distribution transaction capability was released")?
            .verify()?;
        Ok(())
    }

    pub(crate) fn keep(mut self) -> PathBuf {
        self.capability.take();
        std::mem::take(&mut self.path)
    }

    pub(crate) fn cleanup_now(
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

pub(crate) fn validate_commit_location(parent: &Path, destination: &Path) -> Result {
    xlfn_package::validate_directory_path(parent)?;
    validate_output_destination(destination)?;
    Ok(())
}

pub(crate) fn commit_prepared_directory(
    prepared: &xlfn_package::PreparedDirectoryCommit,
    destination: &Path,
    verify_source: impl Fn(&Path) -> Result,
    verify_destination: impl Fn(&Path) -> Result,
) -> Result {
    commit_prepared_directory_with(
        prepared,
        destination,
        verify_source,
        verify_destination,
        &SystemDistributionFileOps,
    )
}

pub(crate) fn commit_prepared_directory_with(
    prepared: &xlfn_package::PreparedDirectoryCommit,
    destination: &Path,
    verify_source: impl Fn(&Path) -> Result,
    verify_destination: impl Fn(&Path) -> Result,
    file_ops: &impl DistributionFileOps,
) -> Result {
    validate_output_destination(destination)?;
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    validate_commit_location(parent, destination)?;
    let destination_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .context("distribution destination name is not valid UTF-8")?;
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
            return Err(anyhow!(
                "distribution commit failed: {commit_error}; recording rollback state also failed: {state_error}"
            ));
        }
        if had_previous {
            let expected_previous = journal_state
                .previous_identity
                .context("transaction has no previous destination identity")
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
                        .context("transaction has no previous destination identity")
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
                return Err(anyhow!(
                    "distribution commit failed: {commit_error}; rollback succeeded but recording its state failed: {state_error}"
                ));
            }
            transaction.cleanup_now(parent, file_ops)?;
        }
        file_ops.sync_directory(parent)?;
        return Err(commit_error.into());
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
        .map_err(anyhow::Error::from)
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
            return Err(anyhow!(
                "post-commit verification failed: {verification_error}; recording rollback state also failed: {state_error}"
            ));
        }
        if had_previous {
            let failed = transaction.path().join("failed-install");
            let expected_installed = journal_state
                .installed_identity
                .context("transaction has no installed destination identity")?;
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
                .context("transaction has no previous destination identity")?;
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
                return Err(anyhow!(
                    "post-commit verification failed: {verification_error}; rollback succeeded but recording its state failed: {state_error}"
                ));
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
            eprintln!(
                "cargo xlfn: warning: committed {} but could not remove backup {}: {error}",
                destination.display(),
                previous.display()
            );
            return Ok(());
        }
        file_ops.sync_directory(transaction.path())?;
    }
    transaction.cleanup_now(parent, file_ops)?;
    Ok(())
}

#[derive(Debug)]
pub(crate) struct IoErrorSource(pub(crate) anyhow::Error);

impl fmt::Display for IoErrorSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for IoErrorSource {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        let error: &(dyn std::error::Error + 'static) = self.0.as_ref();
        error.source()
    }
}

pub(crate) fn error_as_io<E>(error: E) -> io::Error
where
    E: Into<anyhow::Error>,
{
    // Keep the original anyhow chain behind the standard I/O error rather
    // than flattening it to a display string.
    io::Error::other(IoErrorSource(error.into()))
}

pub(crate) fn distribution_recovery_error(
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

pub(crate) fn quarantine_transaction(
    parent: &Path,
    destination_name: &str,
    prefix: &str,
    transaction: &Path,
    reason: impl Into<String>,
    file_ops: &impl DistributionFileOps,
) -> Result {
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

pub(crate) fn recover_stale_transactions(
    parent: &Path,
    destination_name: &str,
    commit_guard: &DistributionCommitGuard,
    file_ops: &impl DistributionFileOps,
) -> Result {
    commit_guard.ensure_held()?;
    if commit_guard.destination() != parent.join(destination_name) {
        bail!("distribution recovery lock does not match its destination");
    }
    xlfn_package::validate_directory_path(parent)?;
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
        xlfn_package::validate_path_components(&transaction)?;
        let private_transaction = is_private_transaction(&transaction, &prefix)?;
        let payloads = transaction_payloads(&transaction)?;
        let journal = transaction.join(TRANSACTION_JOURNAL);
        let next = transaction.join("journal.next");
        xlfn_package::validate_path_components(&journal)?;
        xlfn_package::validate_path_components(&next)?;

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
        xlfn_package::validate_path_components(&previous)?;
        commit_guard.ensure_held()?;
        match journal_state.state {
            TransactionState::Prepared => {
                if optional_directory_identity(&previous)?.is_some() {
                    let expected_previous = journal_state
                        .previous_identity
                        .or(journal_state.destination_identity)
                        .context("prepared transaction has no previous identity")?;
                    require_directory_identity(&previous, expected_previous, "previous backup")?;
                    if optional_directory_identity(&destination)?.is_some() {
                        bail!(
                            "distribution transaction journal is inconsistent: {}",
                            transaction.display()
                        );
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
                    bail!(
                        "distribution transaction journal is inconsistent: {}",
                        transaction.display()
                    );
                }
                if optional_directory_identity(&destination)?.is_some()
                    && journal_state.destination_identity.is_none()
                {
                    bail!(
                        "no-previous transaction has an unexpected destination: {}",
                        transaction.display()
                    );
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
                    bail!(
                        "install-pending transaction lost its backup: {}",
                        transaction.display()
                    );
                }
            }
            TransactionState::InstallPendingNoPrevious => {
                if optional_directory_identity(&previous)?.is_some() {
                    bail!(
                        "distribution transaction journal is inconsistent: {}",
                        transaction.display()
                    );
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
                    bail!(
                        "previous-saved transaction lost its backup: {}",
                        transaction.display()
                    );
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
                    bail!(
                        "installed transaction lost its backup: {}",
                        transaction.display()
                    );
                }
            }
            TransactionState::InstalledNoPrevious | TransactionState::RollbackPendingNoPrevious => {
                if optional_directory_identity(&previous)?.is_some() {
                    bail!(
                        "distribution transaction journal is inconsistent: {}",
                        transaction.display()
                    );
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
                let installed = journal_state
                    .installed_identity
                    .context("committed transaction has no installed identity")?;
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
                    let expected = journal_state
                        .previous_identity
                        .context("committed transaction has no previous identity")?;
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

pub(crate) fn require_original_destination(
    destination: &Path,
    journal: &TransactionJournal,
) -> Result<()> {
    let expected = journal
        .destination_identity
        .context("transaction has no original destination identity")?;
    Ok(require_directory_identity(
        destination,
        expected,
        "restored destination",
    )?)
}

pub(crate) fn remove_installed_distribution_with(
    destination: &Path,
    journal: &TransactionJournal,
    file_ops: &impl DistributionFileOps,
) -> Result<()> {
    if optional_directory_identity(destination)?.is_none() {
        return Ok(());
    }
    let expected = journal
        .installed_identity
        .context("transaction has no installed destination identity")?;
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

pub(crate) fn restore_previous_distribution(
    destination: &Path,
    transaction: &Path,
    previous: &Path,
    journal: &TransactionJournal,
    file_ops: &impl DistributionFileOps,
) -> Result {
    validate_output_destination(destination)?;
    let expected_previous = journal
        .previous_identity
        .or(journal.destination_identity)
        .context("transaction has no previous destination identity")?;
    require_directory_identity(previous, expected_previous, "previous backup")?;
    let recovery_install = transaction.join("recovery-install");
    xlfn_package::validate_path_components(&recovery_install)?;
    if optional_directory_identity(destination)?.is_some() {
        let expected_installed = journal
            .installed_identity
            .context("transaction has no installed destination identity")?;
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
            let expected_installed = journal
                .installed_identity
                .context("transaction has no installed identity")?;
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

pub(crate) fn remove_recovery_install(
    transaction: &Path,
    journal: &TransactionJournal,
    file_ops: &impl DistributionFileOps,
) -> Result {
    let recovery_install = transaction.join("recovery-install");
    xlfn_package::validate_path_components(&recovery_install)?;
    if optional_directory_identity(&recovery_install)?.is_some() {
        let expected = journal
            .installed_identity
            .context("transaction has no installed identity for recovery installation")?;
        require_directory_identity(&recovery_install, expected, "recovery installation")?;
        file_ops.remove_dir_all(&recovery_install)?;
        file_ops.sync_directory(transaction)?;
    }
    Ok(())
}
