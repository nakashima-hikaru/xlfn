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
            let verified = stage_package_target(
                *target,
                args,
                &metadata,
                &target_staging,
                build_target_directory,
            )?;
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
        let verified =
            stage_package_target(target, args, &metadata, &staging, build_target_directory)?;
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

pub(crate) fn stage_package_target(
    target: WindowsTarget,
    args: &PackageArgs,
    metadata: &ProjectMetadata,
    staging: &xlfn_package::PrivateStagingDirectory,
    target_directory: &Path,
) -> Result<xlfn_package::VerifiedPackage> {
    let build = args.build();
    let profile = build.profile.as_deref().unwrap_or("release");
    let mut command = cargo_command();
    command
        .args(["build", "--manifest-path"])
        .arg(&metadata.manifest_path)
        .args([
            "--package",
            &metadata.package_name,
            "--target",
            target.triple(),
        ]);
    configure_build(&mut command, metadata, target.triple(), target_directory)?;
    build.apply_to_command(&mut command, Some("release"));
    if !command.status()?.success() {
        bail!("cargo build failed for {}", target.triple());
    }
    let source = built_library_path(
        metadata,
        target.triple(),
        &build,
        Some("release"),
        target_directory,
    );
    if !source.is_file() {
        bail!("built XLL DLL was not found at {}", source.display());
    }
    let source_snapshot = xlfn_package::snapshot_file(target.triple(), &source)?;
    let (bundle, strict_paths) = match &metadata.bundle {
        Some(bundle_metadata) => (
            xlfn_package::resolve_bundle_files_with_metadata(
                &metadata.manifest_directory,
                target.triple(),
                bundle_metadata,
            )?,
            bundle_metadata.strict_paths,
        ),
        None => (xlfn_package::ResolvedBundle::empty(), true),
    };
    validate_bundle_output_names(&bundle, &metadata.artifact_name)?;
    let bundle_sources = bundle
        .resolved_files()
        .map(|(configured_path, staged_source)| -> Result<_> {
            let staged_relative_path = staged_source
                .file_name()
                .and_then(|name| name.to_str())
                .context("bundle file basename is not valid UTF-8")?;
            Ok(json!({
                "configured_path": configured_path,
                "staged_relative_path": staged_relative_path,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let external_imports = bundle.external_imports().collect::<Vec<_>>();

    let validation_staging = xlfn_package::PrivateStagingDirectory::create(
        &staging.path().with_added_extension("validation"),
    )?;
    let mut staged_bundle = xlfn_package::stage_bundle(&bundle, &validation_staging)?;
    let xll = validation_staging
        .path()
        .join(format!("{}.xll", metadata.artifact_name));
    if fs::symlink_metadata(&xll).is_ok() {
        bail!(
            "bundle basename collides with generated XLL: {}",
            xll.display()
        );
    }
    fs::write(&xll, source_snapshot.as_ref())?;

    let observation = CrtObservation::inspect(&xlfn_package::inspect_pe(&xll)?, metadata.crt)?;
    observation.warn_if_mixed();
    // Keep the closed-world verifier strict for arbitrary imports while
    // permitting only dynamic CRT names observed in this exact XLL image.
    staged_bundle.try_add_external_imports(&observation.observed_dynamic_crt_imports)?;

    // Inspect only the isolated files. These same staged bytes are hashed
    // below and become the committed distribution directory.
    let verified = xlfn_package::verify_staged_package(&xll, target.triple(), &[], staged_bundle)?;

    let files = verified
        .artifacts()
        .iter()
        .map(|artifact| {
            json!({
                "relative_path": artifact.relative_path().to_string_lossy(),
                "size": artifact.size(),
                "sha256": artifact.sha256_hex(),
            })
        })
        .collect::<Vec<_>>();
    let manifest = json!({
        "schema": 6,
        "package": metadata.package_name,
        "package_version": metadata.package_version,
        "artifact": metadata.artifact_name,
        "target": target.triple(),
        "profile": profile,
        "feature_selection": {
            "explicit": &build.features,
            "default_features": !build.no_default_features,
            "all_features": build.all_features,
            "resolved": &metadata.resolved_features,
        },
        "cargo_constraints": {
            "locked": build.locked,
            "frozen": build.frozen,
            "offline": build.offline,
            "lockfile_sha256": &metadata.lockfile_sha256,
        },
        "crt": observation.manifest(metadata.crt),
        "bundle_sources": bundle_sources,
        "bundle_policy": {
            "strict_paths": strict_paths,
            "system_import_policy": xlfn_package::SYSTEM_IMPORT_POLICY_VERSION,
            "external_imports": external_imports,
        },
        "integrity": {
            "purpose": "audit-metadata-only",
            "runtime_verified": false,
            "trust_boundary": "protected-install-location-and-native-code-signing",
        },
        "files": files,
    });
    let verified = verified.with_manifest_bytes(serde_json::to_vec_pretty(&manifest)?)?;
    verified.materialize(staging)?;
    fs::remove_dir_all(validation_staging.path())?;
    Ok(verified)
}
