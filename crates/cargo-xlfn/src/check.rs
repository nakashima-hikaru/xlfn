use super::*;

pub(crate) fn check(args: &CheckArgs) -> Result {
    let project = args.project();
    let build = args.build();
    let metadata = project_metadata(&project, &build)?;
    metadata.crt.print();
    let target_directory = metadata.crt.target_directory(&metadata.target_directory);
    let only_target = args.target.map(WindowsTarget::triple);
    let rust_targets = only_target.map_or_else(
        || vec![WindowsTarget::X86.triple(), WindowsTarget::X64.triple()],
        |target| vec![target],
    );
    for target in &rust_targets {
        let mut command = cargo_command();
        command
            .args(["build", "--manifest-path"])
            .arg(&metadata.manifest_path)
            .args(["--package", &metadata.package_name])
            .args(["--target", target]);
        configure_build(&mut command, &metadata, target, &target_directory)?;
        build.apply_to_command(&mut command, None);
        if !command.status()?.success() {
            bail!("XLL001 Rust build/link failed for {target}");
        }

        let source = built_library_path(&metadata, target, &build, None, &target_directory);
        if !source.is_file() {
            bail!("built XLL DLL was not found at {}", source.display());
        }
        let source_snapshot = xlfn_package::snapshot_file(target, &source)?;
        let bundle = match &metadata.bundle {
            Some(bundle_metadata) => xlfn_package::resolve_bundle_files_with_metadata(
                &metadata.manifest_directory,
                target,
                bundle_metadata,
            )?,
            None => xlfn_package::ResolvedBundle::empty(),
        };
        validate_bundle_output_names(&bundle, &metadata.artifact_name)?;
        let staging_guard = tempfile::Builder::new()
            .prefix(".cargo-xlfn-check-")
            .tempdir()?;
        let package = staging_guard.path().join("package");
        let package_staging = xlfn_package::PrivateStagingDirectory::create(&package)?;
        let mut staged_bundle = xlfn_package::stage_bundle(&bundle, &package_staging)?;
        let xll = package_staging
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
        // Dynamic MSVC runtimes are redistributable external dependencies,
        // not Windows inbox DLLs. Admit only names classified from the actual
        // PE import table by the CRT observer.
        staged_bundle.try_add_external_imports(&observation.observed_dynamic_crt_imports)?;
        xlfn_package::verify_staged_package(&xll, target, &[], staged_bundle)?;
    }
    println!("Cargo manifest / cdylib  OK");
    println!("Rust build and link       OK ({})", rust_targets.join(", "));
    println!("XLL exports / manifest    OK");
    println!("PE architecture/imports   OK");
    Ok(())
}
