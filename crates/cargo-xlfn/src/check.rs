use super::*;

pub(crate) fn check(args: &CheckArgs) -> Result {
    let project = args.project();
    let build = args.build();
    let metadata = project_metadata(&project, &build)?;
    metadata.crt.print();
    let target_directory = metadata.crt.target_directory(&metadata.target_directory);
    let targets = args.target.map_or_else(
        || vec![WindowsTarget::X86, WindowsTarget::X64],
        |target| vec![target],
    );
    for target in &targets {
        let _verified = build_and_verify_target(TargetBuildRequest {
            target: *target,
            metadata: &metadata,
            build: &build,
            default_profile: DefaultBuildProfile::Dev,
            target_directory: &target_directory,
        })?;
    }
    println!("Cargo manifest / cdylib  OK");
    let target_names = targets
        .iter()
        .map(|target| target.triple())
        .collect::<Vec<_>>()
        .join(", ");
    println!("Rust build and link       OK ({target_names})");
    println!("XLL exports / manifest    OK");
    println!("PE architecture/imports   OK");
    Ok(())
}
