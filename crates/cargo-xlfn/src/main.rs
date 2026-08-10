//! Cargo subcommand for checking, staging, and packaging Rust Excel XLLs.
//!
//! `cargo xlfn check` validates the selected Windows target and CRT policy,
//! while `cargo xlfn package` builds a closed-world package and commits it through
//! the transactional staging API in `xlfn-package`.

use anyhow::{Context, anyhow, bail};
use cargo_metadata::{CargoOpt, Metadata, MetadataCommand, Package};
use clap::{Args, Parser, Subcommand, ValueEnum};
use fs_err as fs;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use xlfn_package::{BundleMetadata, validate_windows_basename};

#[cfg(target_os = "windows")]
#[allow(
    clippy::undocumented_unsafe_blocks,
    reason = "FFI bindings in win32 module"
)]
mod win32;

mod crt;

use crt::{CrtObservation, CrtPolicy, ResolvedCrtPolicy};

type Result<T = ()> = anyhow::Result<T>;

fn main() {
    if crt::wrapper_mode_requested() {
        match crt::run_wrapper() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(error) => {
                eprintln!("cargo xlfn rustc wrapper: {error:#}");
                std::process::exit(1);
            }
        }
    }
    if let Err(error) = run() {
        eprintln!("cargo xlfn: {error}");
        std::process::exit(1);
    }
}

#[derive(Parser)]
#[command(name = "cargo xlfn", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Check(CheckArgs),
    Package(PackageArgs),
}

#[derive(Args)]
struct CheckArgs {
    #[command(flatten)]
    project: ProjectArgs,
    #[command(flatten)]
    build: BuildSelectionArgs,
    #[arg(long, value_enum)]
    target: Option<WindowsTarget>,
}

#[derive(Args, Clone, Debug, Default)]
struct BuildSelectionArgs {
    /// MSVC CRT policy for target Rust crates.
    #[arg(long, value_enum)]
    crt: Option<CrtPolicy>,
    /// Base Cargo target directory; the CRT policy is appended to this path.
    #[arg(long)]
    target_dir: Option<PathBuf>,
    #[arg(long)]
    profile: Option<String>,
    #[arg(long, value_delimiter = ',')]
    features: Vec<String>,
    #[arg(long)]
    no_default_features: bool,
    #[arg(long)]
    all_features: bool,
    #[arg(long)]
    locked: bool,
    #[arg(long)]
    frozen: bool,
    #[arg(long)]
    offline: bool,
}

impl BuildSelectionArgs {
    fn apply_to_command(&self, command: &mut Command, default_profile: Option<&str>) {
        let profile = self.profile.as_deref().or(default_profile);
        if let Some(profile) = profile {
            command.arg("--profile").arg(profile);
        }
        if !self.features.is_empty() {
            command.arg("--features").arg(self.features.join(","));
        }
        if self.no_default_features {
            command.arg("--no-default-features");
        }
        if self.all_features {
            command.arg("--all-features");
        }
        if self.locked {
            command.arg("--locked");
        }
        if self.frozen {
            command.arg("--frozen");
        }
        if self.offline {
            command.arg("--offline");
        }
    }

    fn apply_to_metadata(&self, command: &mut MetadataCommand) {
        if !self.features.is_empty() {
            command.features(CargoOpt::SomeFeatures(self.features.clone()));
        }
        if self.no_default_features {
            command.features(CargoOpt::NoDefaultFeatures);
        }
        if self.all_features {
            command.features(CargoOpt::AllFeatures);
        }
        self.apply_resolution_constraints(command);
    }

    fn apply_resolution_constraints(&self, command: &mut MetadataCommand) {
        let mut other_options = Vec::new();
        if self.locked {
            other_options.push("--locked".to_owned());
        }
        if self.frozen {
            other_options.push("--frozen".to_owned());
        }
        if self.offline {
            other_options.push("--offline".to_owned());
        }
        if !other_options.is_empty() {
            command.other_options(other_options);
        }
    }
}

#[derive(Args, Default)]
struct ProjectArgs {
    #[arg(long)]
    manifest_path: Option<PathBuf>,
    #[arg(long)]
    package: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum WindowsTarget {
    #[value(name = "i686-pc-windows-msvc")]
    X86,
    #[value(name = "x86_64-pc-windows-msvc")]
    X64,
}

impl WindowsTarget {
    const fn triple(self) -> &'static str {
        match self {
            Self::X86 => "i686-pc-windows-msvc",
            Self::X64 => "x86_64-pc-windows-msvc",
        }
    }

    const fn directory(self) -> &'static str {
        match self {
            Self::X86 => "win-x86",
            Self::X64 => "win-x64",
        }
    }
}

#[derive(Args)]
struct PackageArgs {
    #[arg(long, value_enum, conflicts_with = "all")]
    target: Option<WindowsTarget>,
    #[arg(long, conflicts_with = "target")]
    all: bool,
    #[arg(long, default_value = "package")]
    out: PathBuf,
    #[command(flatten)]
    project: ProjectArgs,
    #[command(flatten)]
    build: BuildSelectionArgs,
}

fn normalize_cargo_subcommand_args(mut args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    if args.get(1).is_some_and(|arg| arg == "xlfn") {
        args.remove(1);
    }
    args
}

fn run() -> Result {
    let args = normalize_cargo_subcommand_args(std::env::args_os().collect());
    match Cli::parse_from(args).command {
        Commands::Check(args) => check(&args),
        Commands::Package(args) => package(&args),
    }
}

fn check(args: &CheckArgs) -> Result {
    let metadata = project_metadata(&args.project, &args.build)?;
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
        args.build.apply_to_command(&mut command, None);
        if !command.status()?.success() {
            bail!("XLL001 Rust build/link failed for {target}");
        }

        let source = built_library_path(&metadata, target, &args.build, None, &target_directory);
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

fn built_library_path(
    metadata: &ProjectMetadata,
    target: &str,
    build: &BuildSelectionArgs,
    default_profile: Option<&str>,
    target_directory: &Path,
) -> PathBuf {
    let profile = build
        .profile
        .as_deref()
        .or(default_profile)
        .unwrap_or("dev");
    let profile_directory = if profile == "dev" { "debug" } else { profile };
    target_directory
        .to_path_buf()
        .join(target)
        .join(profile_directory)
        .join(format!("{}.dll", metadata.lib_name.replace('-', "_")))
}

fn validate_bundle_output_names(
    bundle: &xlfn_package::ResolvedBundle,
    artifact_name: &str,
) -> Result {
    for (configured_path, source) in bundle.resolved_files() {
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .context("bundle file basename is not valid UTF-8")?;
        if is_reserved_distribution_name(name, artifact_name) {
            bail!("bundle file {configured_path:?} uses reserved distribution basename {name:?}");
        }
    }
    Ok(())
}

fn is_reserved_distribution_name(name: &str, artifact_name: &str) -> bool {
    name.eq_ignore_ascii_case(&format!("{artifact_name}.xll"))
        || name.eq_ignore_ascii_case("build-manifest.json")
}

#[cfg(target_os = "windows")]
fn retryable_windows_path_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
    ) || matches!(error.raw_os_error(), Some(5) | Some(32) | Some(33))
}

#[cfg(target_os = "windows")]
fn retry_windows_path_operation(mut operation: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    const ATTEMPTS: usize = 24;
    let mut delay = std::time::Duration::from_millis(10);
    for attempt in 0..ATTEMPTS {
        match operation() {
            Ok(()) => return Ok(()),
            Err(error) if attempt + 1 < ATTEMPTS && retryable_windows_path_error(&error) => {
                // Virus scanners and indexing services on hosted Windows runners can
                // briefly retain a handle after the writer closes it. Keep retries
                // bounded so persistent ACL failures remain visible.
                std::thread::sleep(delay);
                delay = delay
                    .saturating_mul(2)
                    .min(std::time::Duration::from_millis(500));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded path-operation loop always returns")
}

#[cfg(target_os = "windows")]
fn move_file_ex_with_retry(
    from: &Path,
    to: &Path,
    flags: crate::win32::MOVE_FILE_FLAGS,
) -> io::Result<()> {
    use crate::win32::MoveFileExW;
    use std::os::windows::ffi::OsStrExt;

    let from_wide = from
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let to_wide = to
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    retry_windows_path_operation(|| {
        // SAFETY: both paths are live, NUL-terminated buffers for this call.
        if unsafe { MoveFileExW(from_wide.as_ptr(), to_wide.as_ptr(), flags) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

fn rename_path(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        // std::fs::rename can use the newer Windows rename-by-handle path when
        // MoveFileExW alone is rejected, while preserving same-volume rename
        // semantics. The bounded retry still covers transient scanner locks.
        retry_windows_path_operation(|| fs::rename(from, to))
    }

    #[cfg(not(target_os = "windows"))]
    fs::rename(from, to)
}

struct ProjectMetadata {
    package_name: String,
    package_version: String,
    lib_name: String,
    artifact_name: String,
    manifest_path: PathBuf,
    manifest_directory: PathBuf,
    target_directory: PathBuf,
    crt: ResolvedCrtPolicy,
    resolved_features: Vec<String>,
    lockfile_sha256: Option<String>,
    bundle: Option<BundleMetadata>,
}

fn project_metadata(args: &ProjectArgs, build: &BuildSelectionArgs) -> Result<ProjectMetadata> {
    // Discover the selected package without resolving dependencies or applying
    // package-relative feature names. `cargo metadata` has no `--package` option,
    // so an unqualified `--features foo` at a virtual workspace root can select
    // the wrong package or fail before the requested member is known.
    let mut discovery_command = MetadataCommand::new();
    discovery_command.no_deps();
    if let Some(path) = &args.manifest_path {
        discovery_command.manifest_path(path);
    }
    build.apply_resolution_constraints(&mut discovery_command);
    let discovery = discovery_command.exec()?;
    let current_directory = std::env::current_dir()?;
    let selected = select_discovery_package(&discovery, args, &current_directory)?;
    let selected_id = selected.id.clone();
    let selected_manifest = selected.manifest_path.clone();

    // Resolve again from the selected member manifest with exactly the feature
    // and lock/network constraints that will be passed to `cargo build`.
    let mut command = MetadataCommand::new();
    command.manifest_path(selected_manifest.as_std_path());
    build.apply_to_metadata(&mut command);
    let cargo = command.exec()?;
    let package = cargo
        .packages
        .iter()
        .find(|package| package.id == selected_id)
        .context("selected package disappeared from resolved Cargo metadata")?;
    let libraries = package
        .targets
        .iter()
        .filter(|target| target.kind.iter().any(|kind| kind.to_string() == "cdylib"))
        .collect::<Vec<_>>();
    if libraries.len() != 1 {
        bail!(
            "package {} must have exactly one cdylib target, found {}",
            package.name,
            libraries.len()
        );
    }
    let metadata = package.metadata.get("xlfn");
    let metadata_crt = metadata
        .and_then(|value| value.get("crt"))
        .map(CrtPolicy::parse_metadata)
        .transpose()?;
    let crt = ResolvedCrtPolicy::resolve(build.crt, metadata_crt);
    let artifact_name = metadata
        .and_then(|value| value.get("artifact-name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(package.name.as_str())
        .to_owned();
    validate_windows_basename(&artifact_name)
        .with_context(|| format!("invalid artifact-name {artifact_name:?}"))?;
    let manifest_directory = package
        .manifest_path
        .parent()
        .context("package manifest has no parent")?
        .as_std_path()
        .to_path_buf();
    let bundle = metadata
        .and_then(|value| value.get("bundle"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("invalid [package.metadata.xlfn.bundle]")?;
    let mut resolved_features = cargo
        .resolve
        .as_ref()
        .and_then(|resolve| resolve.nodes.iter().find(|node| node.id == package.id))
        .map(|node| {
            node.features
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    resolved_features.sort_unstable();
    resolved_features.dedup();
    let lockfile = cargo.workspace_root.as_std_path().join("Cargo.lock");
    let lockfile_sha256 = lockfile
        .is_file()
        .then(|| xlfn_package::sha256(&lockfile))
        .transpose()?;
    Ok(ProjectMetadata {
        package_name: package.name.to_string(),
        package_version: package.version.to_string(),
        lib_name: libraries[0].name.clone(),
        artifact_name,
        manifest_path: package.manifest_path.as_std_path().to_path_buf(),
        manifest_directory,
        target_directory: build
            .target_dir
            .clone()
            .unwrap_or_else(|| cargo.target_directory.as_std_path().to_path_buf()),
        crt,
        resolved_features,
        lockfile_sha256,
        bundle,
    })
}

fn select_discovery_package<'a>(
    discovery: &'a Metadata,
    args: &ProjectArgs,
    current_directory: &Path,
) -> Result<&'a Package> {
    if let Some(name) = &args.package {
        return discovery
            .packages
            .iter()
            .find(|package| package.name.as_str() == name)
            .ok_or_else(|| anyhow!("workspace has no package {name:?}"));
    }

    if let Some(path) = &args.manifest_path {
        let canonical_target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        return discovery
            .packages
            .iter()
            .find(|package| {
                let package_manifest = package.manifest_path.as_std_path();
                fs::canonicalize(package_manifest)
                    .map(|canonical| canonical == canonical_target)
                    .unwrap_or(false)
                    || package_manifest == path
            })
            .ok_or_else(|| anyhow!("workspace has no package matching manifest path {path:?}"));
    }

    if let Some(package) = package_for_current_directory(discovery, current_directory) {
        return Ok(package);
    }

    if let Some(root) = discovery.root_package() {
        return Ok(root);
    }

    if discovery.workspace_members.len() == 1 {
        let member_id = &discovery.workspace_members[0];
        return discovery
            .packages
            .iter()
            .find(|package| &package.id == member_id)
            .ok_or_else(|| anyhow!("workspace member package disappeared"));
    }

    bail!("select a workspace member with --package or --manifest-path")
}

fn package_for_current_directory<'a>(
    discovery: &'a Metadata,
    current_directory: &Path,
) -> Option<&'a Package> {
    let current_directory = fs::canonicalize(current_directory).ok()?;
    discovery
        .packages
        .iter()
        .filter(|package| discovery.workspace_members.contains(&package.id))
        .filter_map(|package| {
            let package_directory = package.manifest_path.parent()?.as_std_path();
            let package_directory = fs::canonicalize(package_directory).ok()?;
            current_directory
                .starts_with(&package_directory)
                .then_some((package, package_directory.components().count()))
        })
        .max_by_key(|(_, depth)| *depth)
        .map(|(package, _)| package)
}

fn package(args: &PackageArgs) -> Result {
    let targets = if args.all {
        vec![WindowsTarget::X86, WindowsTarget::X64]
    } else {
        vec![
            args.target
                .context("package requires --target TARGET or --all")?,
        ]
    };
    let metadata = project_metadata(&args.project, &args.build)?;
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
        commit_prepared_directory(
            &prepared_root,
            &args.out,
            |_root| {
                for (_target, prepared) in &prepared_packages {
                    prepared.verify_source_contents()?;
                }
                Ok(())
            },
            |root| {
                prepared_root.verify_committed_contents(root)?;
                for (target, prepared) in &prepared_packages {
                    prepared.verify_committed_contents(&root.join(target.directory()))?;
                }
                Ok(())
            },
        )?;
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
        commit_prepared_directory(
            &prepared_root,
            &destination,
            |_root| Ok(prepared.verify_source_contents()?),
            |root| {
                prepared_root.verify_committed_contents(root)?;
                prepared.verify_committed_contents(root)?;
                Ok(())
            },
        )?;
        println!("created {}", destination.display());
    }
    Ok(())
}

fn validate_transactional_output_root(destination: &Path) -> Result {
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

fn validate_output_destination(destination: &Path) -> Result {
    xlfn_package::validate_directory_path(destination)?;
    Ok(())
}

fn stage_package_target(
    target: WindowsTarget,
    args: &PackageArgs,
    metadata: &ProjectMetadata,
    staging: &xlfn_package::PrivateStagingDirectory,
    target_directory: &Path,
) -> Result<xlfn_package::VerifiedPackage> {
    let profile = args.build.profile.as_deref().unwrap_or("release");
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
    args.build.apply_to_command(&mut command, Some("release"));
    if !command.status()?.success() {
        bail!("cargo build failed for {}", target.triple());
    }
    let source = built_library_path(
        metadata,
        target.triple(),
        &args.build,
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
        &staging.path().with_extension("validation"),
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
            "explicit": &args.build.features,
            "default_features": !args.build.no_default_features,
            "all_features": args.build.all_features,
            "resolved": &metadata.resolved_features,
        },
        "cargo_constraints": {
            "locked": args.build.locked,
            "frozen": args.build.frozen,
            "offline": args.build.offline,
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

trait DistributionFileOps {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;
    fn sync_directory(&self, path: &Path) -> io::Result<()>;
}

struct SystemDistributionFileOps;

impl DistributionFileOps for SystemDistributionFileOps {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        rename_path(from, to)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir_all(path)
    }

    fn sync_directory(&self, path: &Path) -> io::Result<()> {
        sync_directory(path)
    }
}

#[derive(Debug)]
struct DistributionRecoveryError {
    destination: PathBuf,
    commit_error: io::Error,
    rollback_error: io::Error,
    recovery_path: PathBuf,
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
struct DistributionRecoveryQuarantineError {
    transaction: PathBuf,
    quarantine: Option<PathBuf>,
    reason: String,
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

const TRANSACTION_JOURNAL: &str = "journal";
const TRANSACTION_SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum TransactionState {
    Prepared,
    NoPrevious,
    PreviousSaved,
    InstallPending,
    InstallPendingNoPrevious,
    Installed,
    InstalledNoPrevious,
    RollbackPending,
    RollbackPendingNoPrevious,
    RolledBack,
    // The installed directory is authoritative. `previous`, when present,
    // is cleanup payload only and is never a recovery source in this state.
    Committed,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DirectoryIdentity {
    dev: u64,
    ino: u64,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DirectoryIdentity {
    volume_serial_number: u64,
    file_id: [u8; 16],
}

#[cfg(not(any(unix, target_os = "windows")))]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DirectoryIdentity;

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LockFileIdentity {
    dev: u64,
    ino: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TransactionJournal {
    schema: u32,
    transaction_id: String,
    destination_name: String,
    parent_identity: DirectoryIdentity,
    transaction_identity: DirectoryIdentity,
    destination_identity: Option<DirectoryIdentity>,
    state: TransactionState,
    previous_identity: Option<DirectoryIdentity>,
    installed_identity: Option<DirectoryIdentity>,
    sequence: u64,
    checksum: String,
}

impl TransactionJournal {
    fn new(
        transaction_id: String,
        destination_name: String,
        parent_identity: DirectoryIdentity,
        transaction_identity: DirectoryIdentity,
        destination_identity: Option<DirectoryIdentity>,
    ) -> Result<Self> {
        let mut journal = Self {
            schema: TRANSACTION_SCHEMA,
            transaction_id,
            destination_name,
            parent_identity,
            transaction_identity,
            destination_identity,
            state: TransactionState::Prepared,
            previous_identity: None,
            installed_identity: None,
            sequence: 0,
            checksum: String::new(),
        };
        journal.refresh_checksum()?;
        Ok(journal)
    }

    fn refresh_checksum(&mut self) -> Result<()> {
        self.checksum = journal_checksum(self)?;
        Ok(())
    }
}

fn journal_checksum(journal: &TransactionJournal) -> Result<String> {
    let mut unsigned = journal.clone();
    unsigned.checksum.clear();
    let encoded = serde_json::to_vec(&unsigned)?;
    let digest = Sha256::digest(encoded);
    use std::fmt::Write as _;
    let mut checksum = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut checksum, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(checksum)
}

struct DistributionCommitGuard {
    lock_file: std::fs::File,
    lock_path: PathBuf,
    #[cfg(unix)]
    lock_identity: LockFileIdentity,
    destination: PathBuf,
}

impl DistributionCommitGuard {
    fn acquire(parent: &Path, destination_name: &str) -> Result<Self> {
        let lock_path = parent.join(format!(".{destination_name}.lock"));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;

            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }

        #[cfg(target_os = "windows")]
        {
            use crate::win32::FILE_FLAG_OPEN_REPARSE_POINT;
            use std::os::windows::fs::OpenOptionsExt;

            options
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .share_mode(0);
        }

        let lock_file = options.open(&lock_path).with_context(|| {
            format!(
                "failed to open distribution commit lock {}",
                lock_path.display()
            )
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;

            // SAFETY: the file descriptor is valid for the lifetime of the
            // lock file; LOCK_EX serializes operations that use this inode,
            // while ensure_held detects pathname replacement.
            let status = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
            if status != 0 {
                return Err(io::Error::last_os_error()).with_context(|| {
                    format!("failed to lock distribution commit {}", lock_path.display())
                });
            }
        }

        #[cfg(unix)]
        let lock_identity = lock_file_identity(&lock_file)?;
        let guard = Self {
            lock_file,
            lock_path,
            #[cfg(unix)]
            lock_identity,
            destination: parent.join(destination_name),
        };
        guard.ensure_held()?;
        Ok(guard)
    }

    fn ensure_held(&self) -> io::Result<()> {
        let metadata = self.lock_file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::other(
                "distribution commit lock handle is not a file",
            ));
        }

        #[cfg(unix)]
        {
            let path_metadata = fs::symlink_metadata(&self.lock_path)?;
            if !path_metadata.is_file() {
                return Err(io::Error::other(
                    "distribution commit lock path is not a regular file",
                ));
            }
            let path_identity = lock_file_identity_from_metadata(&path_metadata);
            if path_identity != self.lock_identity {
                return Err(io::Error::other(
                    "distribution commit lock path was replaced while held",
                ));
            }
        }

        #[cfg(target_os = "windows")]
        {
            // share_mode(0) on the held handle prevents pathname deletion or
            // replacement while the lock is held; the path check below also
            // detects an externally forced replacement before a destructive
            // phase.
            let path_metadata = fs::symlink_metadata(&self.lock_path)?;
            if !path_metadata.is_file() {
                return Err(io::Error::other(
                    "distribution commit lock path is not a regular file",
                ));
            }
        }

        Ok(())
    }
}

static TRANSACTION_COUNTER: AtomicU64 = AtomicU64::new(0);

struct DistributionTransactionDirectory {
    path: PathBuf,
    capability: Option<xlfn_package::PrivateStagingDirectory>,
}

impl DistributionTransactionDirectory {
    fn create(
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

    fn path(&self) -> &Path {
        &self.path
    }

    fn verify(&self) -> Result<()> {
        self.capability
            .as_ref()
            .context("distribution transaction capability was released")?
            .verify()?;
        Ok(())
    }

    fn keep(mut self) -> PathBuf {
        self.capability.take();
        std::mem::take(&mut self.path)
    }

    fn cleanup_now(mut self, parent: &Path, file_ops: &impl DistributionFileOps) -> io::Result<()> {
        self.capability.take();
        file_ops.remove_dir_all(&self.path)?;
        file_ops.sync_directory(parent)
    }
}

impl Drop for DistributionTransactionDirectory {
    fn drop(&mut self) {}
}

fn directory_identity(path: &Path) -> Result<DirectoryIdentity> {
    xlfn_package::validate_path_components(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        bail!(
            "expected a directory for identity check: {}",
            path.display()
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        Ok(DirectoryIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }

    #[cfg(target_os = "windows")]
    {
        directory_identity_windows(path)
    }

    #[cfg(not(any(unix, target_os = "windows")))]
    {
        Ok(DirectoryIdentity)
    }
}

#[cfg(unix)]
fn lock_file_identity(file: &std::fs::File) -> io::Result<LockFileIdentity> {
    Ok(lock_file_identity_from_metadata(&file.metadata()?))
}

#[cfg(unix)]
fn lock_file_identity_from_metadata(metadata: &std::fs::Metadata) -> LockFileIdentity {
    use std::os::unix::fs::MetadataExt;

    LockFileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

#[cfg(target_os = "windows")]
fn directory_identity_windows(path: &Path) -> Result<DirectoryIdentity> {
    use crate::win32::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdInfo, GetFileInformationByHandleEx, HANDLE,
    };
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    let file = options.open(path)?;
    let mut information = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: file remains open for the duration of the call and information
    // points to writable storage of the documented size.
    let status = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileIdInfo,
            information.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if status == 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: a successful GetFileInformationByHandleEx initializes the output.
    let information = unsafe { information.assume_init() };
    Ok(DirectoryIdentity {
        volume_serial_number: information.VolumeSerialNumber,
        file_id: information.FileId.Identifier,
    })
}

fn optional_directory_identity(path: &Path) -> Result<Option<DirectoryIdentity>> {
    match fs::symlink_metadata(path) {
        Ok(_) => directory_identity(path).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn require_directory_identity(path: &Path, expected: DirectoryIdentity, label: &str) -> Result<()> {
    let actual = directory_identity(path)?;
    if actual != expected {
        bail!("{} identity changed: {}", label, path.display());
    }
    Ok(())
}

fn transaction_id(transaction: &Path, prefix: &str) -> Result<String> {
    let name = transaction
        .file_name()
        .and_then(|name| name.to_str())
        .context("transaction directory name is not valid UTF-8")?;
    let suffix = name
        .strip_prefix(prefix)
        .filter(|id| !id.is_empty())
        .context("transaction directory has an invalid name")?;
    Ok(suffix.strip_prefix("private-").unwrap_or(suffix).to_owned())
}

fn is_private_transaction(transaction: &Path, prefix: &str) -> Result<bool> {
    let name = transaction
        .file_name()
        .and_then(|name| name.to_str())
        .context("transaction directory name is not valid UTF-8")?;
    let suffix = name
        .strip_prefix(prefix)
        .filter(|id| !id.is_empty())
        .context("transaction directory has an invalid name")?;
    Ok(suffix.starts_with("private-"))
}

fn read_transaction_journal(path: &Path) -> Result<TransactionJournal> {
    let bytes = fs::read(path)?;
    let journal: TransactionJournal = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "distribution transaction journal is invalid: {}",
            path.display()
        )
    })?;
    if journal.schema != TRANSACTION_SCHEMA {
        bail!(
            "unsupported distribution transaction journal schema {} in {}",
            journal.schema,
            path.display()
        );
    }
    if journal.sequence == 0 {
        bail!(
            "distribution transaction journal has no committed sequence: {}",
            path.display()
        );
    }
    if journal_checksum(&journal)? != journal.checksum {
        bail!(
            "distribution transaction journal checksum mismatch: {}",
            path.display()
        );
    }
    Ok(journal)
}

fn validate_transaction_provenance(
    parent: &Path,
    destination_name: &str,
    prefix: &str,
    transaction: &Path,
    journal: &TransactionJournal,
) -> Result<xlfn_package::PrivateStagingDirectory> {
    let transaction_directory = xlfn_package::PrivateStagingDirectory::open(transaction)?;
    transaction_directory.verify()?;
    let expected_id = transaction_id(transaction, prefix)?;
    if journal.transaction_id != expected_id {
        bail!(
            "distribution transaction ID does not match its directory: {}",
            transaction.display()
        );
    }
    if journal.destination_name != destination_name {
        bail!(
            "distribution transaction destination does not match its directory: {}",
            transaction.display()
        );
    }
    require_directory_identity(parent, journal.parent_identity, "transaction parent")?;
    require_directory_identity(
        transaction,
        journal.transaction_identity,
        "transaction directory",
    )?;
    Ok(transaction_directory)
}

fn validate_commit_location(parent: &Path, destination: &Path) -> Result {
    xlfn_package::validate_directory_path(parent)?;
    validate_output_destination(destination)?;
    Ok(())
}

fn commit_prepared_directory(
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

fn commit_prepared_directory_with(
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

    if let Err(verification_error) =
        validate_output_destination(destination).and_then(|_| verify_destination(destination))
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
struct IoErrorSource(anyhow::Error);

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

fn error_as_io<E>(error: E) -> io::Error
where
    E: Into<anyhow::Error>,
{
    // Keep the original anyhow chain behind the standard I/O error rather
    // than flattening it to a display string.
    io::Error::other(IoErrorSource(error.into()))
}

fn ensure_destination_absent(destination: &Path) -> Result {
    match fs::symlink_metadata(destination) {
        Ok(_) => bail!("destination unexpectedly exists: {}", destination.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn sync_rename_parents(
    from: &Path,
    to: &Path,
    file_ops: &impl DistributionFileOps,
) -> io::Result<()> {
    // A rename changes both directory entries: persist the source removal and
    // the destination insertion before advancing the transaction state.
    let from_parent = from
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let to_parent = to
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    file_ops.sync_directory(from_parent)?;
    if to_parent != from_parent {
        file_ops.sync_directory(to_parent)?;
    }
    Ok(())
}

fn distribution_recovery_error(
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

fn write_transaction_state(
    journal: &Path,
    parent: &Path,
    transaction: &mut TransactionJournal,
    state: TransactionState,
    file_ops: &impl DistributionFileOps,
) -> io::Result<()> {
    // Journal durability protocol: write the next state, sync the file,
    // atomically replace the journal, then sync both directory entries that
    // make the replacement observable after a power loss.
    transaction.state = state;
    transaction.sequence = transaction
        .sequence
        .checked_add(1)
        .ok_or_else(|| io::Error::other("distribution transaction journal sequence overflow"))?;
    transaction
        .refresh_checksum()
        .map_err(|error| io::Error::other(error.to_string()))?;
    let encoded = serde_json::to_vec_pretty(transaction)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let next = journal.with_file_name(format!("{}.next", TRANSACTION_JOURNAL));
    let mut next_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&next)?;
    next_file.write_all(&encoded)?;
    next_file.sync_all()?;
    drop(next_file);
    atomic_replace_file(&next, journal)?;
    let transaction_directory = journal
        .parent()
        .ok_or_else(|| io::Error::other("transaction journal has no parent directory"))?;
    file_ops.sync_directory(transaction_directory)?;
    file_ops.sync_directory(parent)?;
    Ok(())
}

fn atomic_replace_file(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use crate::win32::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};

        move_file_ex_with_retry(from, to, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)
    }

    #[cfg(not(target_os = "windows"))]
    fs::rename(from, to)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let directory = std::fs::File::open(path)?;
        directory.sync_all()
    }
    #[cfg(target_os = "windows")]
    {
        use crate::win32::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        use std::os::windows::fs::OpenOptionsExt;

        // Windows does not expose a portable parent-directory fsync. File
        // contents are flushed before this call, and every publishing rename
        // uses MoveFileExW with MOVEFILE_WRITE_THROUGH. Reopen the directory
        // to validate that it still resolves to a directory, but do not call
        // File::sync_all: that maps to FlushFileBuffers on a read-only handle
        // and deterministically returns ERROR_ACCESS_DENIED.
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
        let directory = options.open(path)?;
        if !directory.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!(
                    "directory synchronization target is not a directory: {}",
                    path.display()
                ),
            ));
        }
        Ok(())
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory synchronization is unsupported on this platform",
        ))
    }
}

fn transaction_payloads(transaction: &Path) -> Result<Vec<PathBuf>> {
    Ok(fs::read_dir(transaction)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| {
            !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(TRANSACTION_JOURNAL) | Some("journal.next")
            )
        })
        .collect())
}

fn remove_empty_transaction(
    parent: &Path,
    transaction: &Path,
    file_ops: &impl DistributionFileOps,
) -> Result {
    let transaction_directory = xlfn_package::PrivateStagingDirectory::open(transaction)?;
    transaction_directory.verify()?;
    file_ops.remove_dir_all(transaction)?;
    file_ops.sync_directory(parent)?;
    Ok(())
}

fn quarantine_transaction(
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

fn recover_stale_transactions(
    parent: &Path,
    destination_name: &str,
    commit_guard: &DistributionCommitGuard,
    file_ops: &impl DistributionFileOps,
) -> Result {
    commit_guard.ensure_held()?;
    if commit_guard.destination != parent.join(destination_name) {
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

fn require_original_destination(destination: &Path, journal: &TransactionJournal) -> Result<()> {
    let expected = journal
        .destination_identity
        .context("transaction has no original destination identity")?;
    require_directory_identity(destination, expected, "restored destination")
}

fn remove_installed_distribution_with(
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

fn restore_previous_distribution(
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

fn remove_recovery_install(
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

fn cargo_command() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

fn configure_build(
    command: &mut Command,
    metadata: &ProjectMetadata,
    target: &str,
    target_directory: &Path,
) -> Result {
    crt::validate_explicit_policy_target(metadata.crt.policy, target)?;
    command.arg("--target-dir").arg(target_directory);
    crt::configure_wrapper(command, metadata.crt, target)
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn cargo_subcommand_name_is_removed_before_clap_parsing() {
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
        let parsed = Cli::try_parse_from(args).unwrap();
        assert!(matches!(parsed.command, Commands::Check(_)));
    }

    #[test]
    fn removed_native_commands_and_unknown_targets_are_rejected() {
        assert!(Cli::try_parse_from(["cargo-xlfn", "native", "inspect"]).is_err());
        assert!(Cli::try_parse_from(["cargo-xlfn", "new", "my-xll"]).is_err());
        assert!(
            Cli::try_parse_from(["cargo-xlfn", "package", "--target", "x86_64-pc-window-msvc"])
                .is_err()
        );
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
    fn check_args_supports_build_selection_flags() {
        let parsed = Cli::try_parse_from([
            "cargo-xlfn",
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
        assert_eq!(args.build.profile.as_deref(), Some("release"));
        assert_eq!(args.build.features, vec!["feat1", "feat2"]);
        assert_eq!(args.build.crt, Some(CrtPolicy::Dynamic));
        assert!(args.build.locked);
    }

    #[test]
    fn cli_accepts_every_crt_policy() {
        for policy in ["inherit", "static", "dynamic"] {
            assert!(
                Cli::try_parse_from(["cargo-xlfn", "check", "--crt", policy]).is_ok(),
                "policy {policy} should parse"
            );
        }
        assert!(Cli::try_parse_from(["cargo-xlfn", "check", "--crt", "auto"]).is_err());
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
                .windows(2)
                .any(|pair| { pair[0] == "--features" && pair[1] == "feat1,feat2" })
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
            select_discovery_package(&discovery, &args, &workspace.join("crates/xlfn/src"))
                .unwrap();

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
}
