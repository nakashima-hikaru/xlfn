use super::*;

pub(crate) fn cargo_command() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

pub(crate) fn configure_build(
    command: &mut Command,
    metadata: &ProjectMetadata,
    target: &str,
    target_directory: &Path,
) -> Result {
    crt::validate_explicit_policy_target(metadata.crt.policy, target)?;
    command.arg("--target-dir").arg(target_directory);
    let build_dir = metadata
        .crt
        .target_directory(&metadata.target_directory)
        .join("build-cache");
    fs::create_dir_all(&build_dir)?;
    command.env("CARGO_BUILD_BUILD_DIR", build_dir);
    crt::configure_wrapper(command, metadata.crt, target)
}
