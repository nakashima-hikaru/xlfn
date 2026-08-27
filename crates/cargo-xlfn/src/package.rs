use super::*;

pub(crate) fn package(args: &PackageArgs) -> Result {
    let project = args.project();
    let build = args.build();
    let targets = if args.all {
        vec![WindowsTarget::X86, WindowsTarget::X64]
    } else {
        vec![
            args.target
                .context("package requires --target TARGET or --all")?,
        ]
    };
    let metadata = project_metadata(&project, &build)?;
    metadata.crt.print();
    let target_parent = metadata.crt.target_directory(&metadata.target_directory);
    fs::create_dir_all(&target_parent)?;
    let build_target_guard = tempfile::Builder::new()
        .prefix(".cargo-xlfn-package-build-")
        .tempdir_in(&target_parent)?;
    let build_target_directory = build_target_guard.path();
    if args.all {
        validate_transactional_output_root(&args.out)?;
        let output_parent = args
            .out
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(output_parent)?;
        // The parent may have been created after the initial check. Verify
        // the complete path again before placing any staging directory in it.
        validate_output_destination(&args.out)?;
        let staging_guard = tempfile::Builder::new()
            .prefix(".cargo-xlfn-package-all-")
            .tempdir_in(output_parent)?;
        let staging_root = xlfn_package::PrivateStagingDirectory::create(
            &staging_guard.path().join("distribution"),
        )?;
        let mut prepared_packages = Vec::with_capacity(targets.len());
        for target in &targets {
            let target_staging = xlfn_package::PrivateStagingDirectory::create(
                &staging_root.path().join(target.directory()),
            )?;
            validate_output_destination(&args.out.join(target.directory()))?;
            let verified = build_and_verify_target(TargetBuildRequest {
                target: *target,
                metadata: &metadata,
                build: &build,
                default_profile: DefaultBuildProfile::Release,
                target_directory: build_target_directory,
            })?;
            verified.materialize(&target_staging)?;
            let prepared = verified.prepare_commit(target_staging.path(), target.triple())?;
            prepared_packages.push((*target, prepared));
        }
        for target in &targets {
            validate_output_destination(&args.out.join(target.directory()))?;
        }
        let shared_artifacts = prepared_packages
            .iter()
            .flat_map(|(target, prepared)| {
                prepared
                    .shared_artifacts()
                    .map(|(path, bytes)| (PathBuf::from(target.directory()).join(path), bytes))
            })
            .collect::<BTreeMap<_, _>>();
        let prepared_root = xlfn_package::PreparedDirectoryCommit::prepare_with_shared_artifacts(
            staging_root.path(),
            &[
                WindowsTarget::X86.directory(),
                WindowsTarget::X64.directory(),
            ],
            &shared_artifacts,
        )?;
        let mut distribution = xlfn_package::PreparedDistribution::new(prepared_root);
        for (target, prepared) in prepared_packages {
            distribution = distribution.with_nested_package(target.directory(), prepared)?;
        }
        report_distribution_cleanup(distribution.commit(&args.out)?);
        println!("created {}", args.out.display());
    } else {
        validate_output_destination(&args.out)?;
        fs::create_dir_all(&args.out)?;
        validate_output_destination(&args.out)?;
        let target = targets[0];
        let destination = args.out.join(target.directory());
        validate_output_destination(&destination)?;
        let staging_guard = tempfile::Builder::new()
            .prefix(&format!(".{}.tmp-", target.directory()))
            .tempdir_in(&args.out)?;
        let staging = xlfn_package::PrivateStagingDirectory::create(
            &staging_guard.path().join("distribution"),
        )?;
        let verified = build_and_verify_target(TargetBuildRequest {
            target,
            metadata: &metadata,
            build: &build,
            default_profile: DefaultBuildProfile::Release,
            target_directory: build_target_directory,
        })?;
        verified.materialize(&staging)?;
        let prepared = verified.prepare_commit(staging.path(), target.triple())?;
        let expected_names = verified
            .artifacts()
            .iter()
            .map(|artifact| {
                artifact
                    .relative_path()
                    .to_str()
                    .context("verified artifact basename is not valid UTF-8")
            })
            .collect::<Result<Vec<_>>>()?;
        let shared_artifacts = prepared.shared_artifacts().collect::<BTreeMap<_, _>>();
        let prepared_root = xlfn_package::PreparedDirectoryCommit::prepare_with_shared_artifacts(
            staging.path(),
            &expected_names,
            &shared_artifacts,
        )?;
        let distribution =
            xlfn_package::PreparedDistribution::new(prepared_root).with_package(prepared)?;
        report_distribution_cleanup(distribution.commit(&destination)?);
        println!("created {}", destination.display());
    }
    Ok(())
}

fn report_distribution_cleanup(outcome: xlfn_package::CommitOutcome) {
    if let xlfn_package::CleanupOutcome::BackupRetained {
        backup,
        transaction,
        error,
    } = outcome.cleanup()
    {
        eprintln!(
            concat!(
                "cargo xlfn: committed distribution, but could not remove backup {}: {}; ",
                "transaction retained at {}"
            ),
            backup.display(),
            error,
            transaction.display()
        );
    }
}

pub(crate) fn validate_transactional_output_root(destination: &Path) -> Result {
    if !matches!(
        destination.components().next_back(),
        Some(Component::Normal(_))
    ) {
        bail!(
            "package --all output must name a dedicated directory, not {}",
            destination.display()
        );
    }
    validate_output_destination(destination)?;
    if destination.exists() {
        let current = fs::canonicalize(std::env::current_dir()?)?;
        let destination = fs::canonicalize(destination)?;
        if current.starts_with(&destination) {
            bail!(
                "package --all refuses to replace the current directory or one of its ancestors: {}",
                destination.display()
            );
        }
    }
    Ok(())
}
