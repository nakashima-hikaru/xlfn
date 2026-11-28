//! Ownership checks and mutual exclusion for the CLI's shared output tree.

use super::*;
use crate::Architecture;

/// Reserves the entire packaging output root for one operation. Acquire this
/// before creating staging or starting recovery under the root.
///
/// The sidecar lock lives next to the root and survives its replacement.
/// A single-target commit also takes its normal child transaction lock.
pub struct PackageOutputLock {
    root: DistributionCommitGuard,
}

impl PackageOutputLock {
    /// Opens an existing output parent and acquires the root's transaction lock.
    pub fn acquire(root: &Path) -> DistributionResult<Self> {
        let root = std::path::absolute(root)?;
        validate_output_destination(&root)?;
        let parent = root
            .parent()
            .ok_or_else(|| DistributionError::Invalid("package output has no parent".into()))?;
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                DistributionError::Invalid("package output has no UTF-8 basename".into())
            })?;
        Ok(Self {
            root: DistributionCommitGuard::acquire(parent, name)?,
        })
    }

    /// Commits both targets. Only empty roots or inventories produced for this
    /// package may be replaced; unknown entries are never treated as output.
    pub fn commit_all(
        &self,
        prepared: PreparedDistribution,
        package: &str,
    ) -> DistributionResult<CommitOutcome> {
        self.commit(prepared, package, None, &SystemDistributionFileOps)
    }

    /// Checks that a single-target job can safely create staging under the
    /// root. A root transaction could replace that staging during recovery;
    /// only a subsequent all-target command may recover it.
    pub fn ensure_target_staging_allowed(&self) -> DistributionResult<()> {
        self.root.ensure_held()?;
        let root = self.root.destination();
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .expect("validated package output basename");
        let parent = root.parent().expect("validated package output parent");
        let prefix = format!(".{name}.transaction-");
        if let Some(pending) = pending_transaction_paths(parent, &prefix)?
            .next()
            .transpose()?
        {
            return Err(DistributionError::Invalid(format!(
                "unfinished all-target output transaction at {}; retry the previous --all command before packaging a single target",
                pending.display()
            )));
        }
        Ok(())
    }

    /// Commits one target without replacing its sibling architecture.
    pub fn commit_target(
        &self,
        prepared: PreparedDistribution,
        package: &str,
        target: &str,
    ) -> DistributionResult<CommitOutcome> {
        self.commit(
            prepared,
            package,
            Some(Architecture::parse(target)?),
            &SystemDistributionFileOps,
        )
    }

    fn commit(
        &self,
        prepared: PreparedDistribution,
        package: &str,
        architecture: Option<Architecture>,
        file_ops: &impl DistributionFileOps,
    ) -> DistributionResult<CommitOutcome> {
        let root = self.root.destination();
        let child;
        let destination = if let Some(architecture) = architecture {
            self.ensure_target_staging_allowed()?;
            child = root.join(architecture.directory_name());
            &child
        } else {
            root
        };
        prepared.commit_with_policy(
            destination,
            file_ops,
            Some(ManagedOutput {
                root: &self.root,
                package,
                architecture,
            }),
        )
    }
}

impl Architecture {
    fn directory_name(self) -> &'static str {
        match self {
            Self::X86 => "win-x86",
            Self::X64 => "win-x64",
        }
    }

    fn target_triple(self) -> &'static str {
        match self {
            Self::X86 => "i686-pc-windows-msvc",
            Self::X64 => "x86_64-pc-windows-msvc",
        }
    }
}

pub(super) struct ManagedOutput<'a> {
    pub(super) root: &'a DistributionCommitGuard,
    package: &'a str,
    architecture: Option<Architecture>,
}

impl ManagedOutput<'_> {
    pub(super) fn is_root(&self) -> bool {
        self.architecture.is_none()
    }

    pub(super) fn validate_existing(&self, destination: &Path) -> DistributionResult<()> {
        let validate = || -> PackageResult {
            if let Some(architecture) = self.architecture {
                return crate::manifest::validate_existing_package(
                    destination,
                    self.package,
                    architecture.target_triple(),
                );
            }
            // Two architecture directories and their optional persistent
            // lock files (a prior single-target invocation may leave them).
            let entries = crate::commit::read_directory_entries(destination, 4)?;
            for (key, name) in entries {
                let path = destination.join(&name);
                let metadata = fs::symlink_metadata(&path)?;
                let managed = match key.as_str() {
                    "win-x86" => Some(Architecture::X86),
                    "win-x64" => Some(Architecture::X64),
                    ".win-x86.lock" | ".win-x64.lock"
                        if metadata.is_file()
                            && !crate::is_reparse_point(&metadata)
                            && metadata.len() == 0 =>
                    {
                        continue;
                    }
                    _ => None,
                };
                if let Some(architecture) = managed
                    && metadata.is_dir()
                    && !crate::is_reparse_point(&metadata)
                {
                    crate::manifest::validate_existing_package(
                        &path,
                        self.package,
                        architecture.target_triple(),
                    )?;
                } else {
                    return Err(format!("unexpected entry {}", path.display()).into());
                }
            }
            Ok(())
        };
        validate().map_err(|error| {
            DistributionError::Invalid(format!(
                "refusing to replace existing package output at {}: {error}. Choose an empty or new --out directory, or move unrelated files aside",
                destination.display()
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuildManifest, BuildManifestInput, PreparedDirectoryCommit, VerifiedArtifact};

    fn write_target(path: &Path, architecture: Architecture, body: &[u8]) {
        fs::create_dir(path).unwrap();
        let xll = path.join("test.xll");
        fs::write(&xll, body).unwrap();
        let manifest = BuildManifest::from_input(
            BuildManifestInput {
                package: "test-addon".to_owned(),
                artifact: "test".to_owned(),
                target: architecture.target_triple().to_owned(),
                ..BuildManifestInput::default()
            },
            &[VerifiedArtifact::new(
                "test.xll".into(),
                body.to_vec().into(),
                fs::metadata(&xll).unwrap().permissions(),
            )],
        )
        .unwrap();
        fs::write(
            path.join("build-manifest.json"),
            manifest.to_bytes().unwrap(),
        )
        .unwrap();
    }

    fn one(path: &Path) -> PreparedDistribution {
        PreparedDistribution::new(
            PreparedDirectoryCommit::prepare(path, &["test.xll", "build-manifest.json"]).unwrap(),
        )
    }

    fn both(path: &Path) -> PreparedDistribution {
        PreparedDistribution::new(
            PreparedDirectoryCommit::prepare(path, &["win-x86", "win-x64"]).unwrap(),
        )
    }

    #[test]
    fn preserves_foreign_root_and_unlisted_target_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("package");
        let child = root.join("win-x86");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("notes.txt"), b"not package output").unwrap();
        let stage = temp.path().join("all");
        fs::create_dir(&stage).unwrap();
        write_target(&stage.join("win-x86"), Architecture::X86, b"x86 new");
        write_target(&stage.join("win-x64"), Architecture::X64, b"x64 new");
        let lock = PackageOutputLock::acquire(&root).unwrap();
        let error = lock.commit_all(both(&stage), "test-addon").unwrap_err();
        assert!(error.to_string().contains("refusing to replace"));
        assert_eq!(
            fs::read(root.join("notes.txt")).unwrap(),
            b"not package output"
        );
        assert!(!child.exists());

        write_target(&child, Architecture::X86, b"old");
        fs::write(child.join("notes.txt"), b"keep me").unwrap();
        let single = temp.path().join("single");
        write_target(&single, Architecture::X86, b"new");
        let error = lock
            .commit_target(
                one(&single),
                "test-addon",
                Architecture::X86.target_triple(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("refusing to replace"));
        assert_eq!(fs::read(child.join("test.xll")).unwrap(), b"old");
        assert_eq!(fs::read(child.join("notes.txt")).unwrap(), b"keep me");
    }

    #[test]
    fn replaces_existing_packages_across_modes_without_removing_sibling() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("package");
        fs::create_dir(&root).unwrap();
        let lock = PackageOutputLock::acquire(&root).unwrap();
        let single = temp.path().join("single");
        write_target(&single, Architecture::X86, b"old");
        lock.commit_target(
            one(&single),
            "test-addon",
            Architecture::X86.target_triple(),
        )
        .unwrap();
        assert!(root.join(".win-x86.lock").is_file());
        let stage = temp.path().join("all");
        fs::create_dir(&stage).unwrap();
        write_target(&stage.join("win-x86"), Architecture::X86, b"x86");
        write_target(&stage.join("win-x64"), Architecture::X64, b"x64");
        lock.commit_all(both(&stage), "test-addon").unwrap();
        assert_eq!(fs::read(root.join("win-x86/test.xll")).unwrap(), b"x86");

        write_target(&single, Architecture::X86, b"x86 updated");
        assert!(
            lock.commit_target(
                one(&single),
                "foreign-addon",
                Architecture::X86.target_triple()
            )
            .is_err()
        );
        lock.commit_target(
            one(&single),
            "test-addon",
            Architecture::X86.target_triple(),
        )
        .unwrap();
        assert_eq!(
            fs::read(root.join("win-x86/test.xll")).unwrap(),
            b"x86 updated"
        );
        assert_eq!(fs::read(root.join("win-x64/test.xll")).unwrap(), b"x64");
    }

    #[test]
    fn rolls_back_if_a_foreign_file_arrives_at_the_rename_boundary() {
        struct AddFileBeforeRename(PathBuf);
        impl DistributionFileOps for AddFileBeforeRename {
            fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
                if from == self.0 && to.file_name().is_some_and(|name| name == "previous") {
                    fs::write(from.join("new-notes.txt"), b"preserve this")?;
                }
                SystemDistributionFileOps.rename(from, to)
            }
            fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
                SystemDistributionFileOps.remove_dir_all(path)
            }
            fn sync_directory(&self, path: &Path) -> io::Result<()> {
                SystemDistributionFileOps.sync_directory(path)
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("package");
        let child = root.join("win-x86");
        fs::create_dir(&root).unwrap();
        write_target(&child, Architecture::X86, b"old");
        let stage = temp.path().join("single");
        write_target(&stage, Architecture::X86, b"new");
        let lock = PackageOutputLock::acquire(&root).unwrap();
        let error = lock
            .commit(
                one(&stage),
                "test-addon",
                Some(Architecture::X86),
                &AddFileBeforeRename(child.clone()),
            )
            .unwrap_err();
        assert!(error.to_string().contains("refusing to replace"));
        assert_eq!(fs::read(child.join("test.xll")).unwrap(), b"old");
        assert_eq!(
            fs::read(child.join("new-notes.txt")).unwrap(),
            b"preserve this"
        );
    }

    #[test]
    fn preserves_unmanaged_backups_left_by_older_commits() {
        struct LeaveBackup;
        impl DistributionFileOps for LeaveBackup {
            fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
                SystemDistributionFileOps.rename(from, to)
            }
            fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
                if path.file_name().is_some_and(|name| name == "previous") {
                    return Err(io::Error::other("keep the legacy backup"));
                }
                SystemDistributionFileOps.remove_dir_all(path)
            }
            fn sync_directory(&self, path: &Path) -> io::Result<()> {
                SystemDistributionFileOps.sync_directory(path)
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("package");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("notes.txt"), b"keep me").unwrap();
        let stage = temp.path().join("all");
        fs::create_dir(&stage).unwrap();
        write_target(&stage.join("win-x86"), Architecture::X86, b"old");
        write_target(&stage.join("win-x64"), Architecture::X64, b"old");
        let legacy = both(&stage).commit_with(&root, &LeaveBackup).unwrap();
        assert!(matches!(
            legacy.cleanup(),
            CleanupOutcome::BackupRetained { .. }
        ));

        fs::create_dir(&stage).unwrap();
        write_target(&stage.join("win-x86"), Architecture::X86, b"new");
        write_target(&stage.join("win-x64"), Architecture::X64, b"new");
        let lock = PackageOutputLock::acquire(&root).unwrap();
        assert!(lock.ensure_target_staging_allowed().is_err());
        let error = lock.commit_all(both(&stage), "test-addon").unwrap_err();
        let DistributionError::Quarantined(quarantine) = error else {
            panic!("unexpected recovery result: {error}");
        };
        let quarantined = quarantine.quarantine.expect("unproven backup moved aside");
        assert_eq!(
            fs::read(quarantined.join("previous/notes.txt")).unwrap(),
            b"keep me"
        );
        lock.ensure_target_staging_allowed().unwrap();
        assert_eq!(fs::read(root.join("win-x86/test.xll")).unwrap(), b"old");
        lock.commit_all(both(&stage), "test-addon").unwrap();
        assert_eq!(fs::read(root.join("win-x86/test.xll")).unwrap(), b"new");
        assert_eq!(
            fs::read(quarantined.join("previous/notes.txt")).unwrap(),
            b"keep me"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn root_lock_refuses_a_simultaneous_packager() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("package");
        let owner = PackageOutputLock::acquire(&root).unwrap();
        assert!(PackageOutputLock::acquire(&root).is_err());
        drop(owner);
        PackageOutputLock::acquire(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn competing_modes_cannot_stage_while_root_lock_is_held() {
        use std::sync::mpsc;
        use std::time::Duration;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("package");
        let lock = PackageOutputLock::acquire(&root).unwrap();
        let (starting_tx, starting_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let competing_root = root.clone();
        let competing = std::thread::spawn(move || {
            starting_tx.send(()).unwrap();
            let _other = PackageOutputLock::acquire(&competing_root).unwrap();
            acquired_tx.send(()).unwrap();
        });
        starting_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let before_release = acquired_rx.recv_timeout(Duration::from_millis(100));
        drop(lock);
        assert!(matches!(
            before_release,
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        acquired_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        competing.join().unwrap();
    }
}
