use anyhow::{Context, anyhow, bail};
use cargo_metadata::{CargoOpt, Metadata, MetadataCommand, Package};
use clap::{Args, Parser, Subcommand, ValueEnum};
use fs_err as fs;
use serde_json::json;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use xlfn_package::{BundleMetadata, validate_windows_basename};

type Result<T = ()> = anyhow::Result<T>;

fn main() {
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
    New(NewArgs),
    Check(CheckArgs),
    Dist(DistArgs),
}

#[derive(Args)]
struct NewArgs {
    name: PathBuf,
    /// Include empty XLL bundle metadata and vendor directories.
    #[arg(long)]
    bundle: bool,
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
struct DistArgs {
    #[arg(long, value_enum, conflicts_with = "all")]
    target: Option<WindowsTarget>,
    #[arg(long, conflicts_with = "target")]
    all: bool,
    #[arg(long, default_value = "dist")]
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
        Commands::New(args) => scaffold(&args.name, args.bundle),
        Commands::Check(args) => check(&args),
        Commands::Dist(args) => distribute(&args),
    }
}

fn check(args: &CheckArgs) -> Result {
    let metadata = project_metadata(&args.project, &args.build)?;
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
        args.build.apply_to_command(&mut command, None);
        if !command.status()?.success() {
            bail!("XLL001 Rust build/link failed for {target}");
        }

        let source = built_library_path(&metadata, target, &args.build, None);
        if !source.is_file() {
            bail!("built XLL DLL was not found at {}", source.display());
        }
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
        let staged_bundle = xlfn_package::stage_bundle(&bundle, &package)?;
        let xll = package.join(format!("{}.xll", metadata.artifact_name));
        if fs::symlink_metadata(&xll).is_ok() {
            bail!(
                "bundle basename collides with generated XLL: {}",
                xll.display()
            );
        }
        fs::copy(&source, &xll)?;
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
) -> PathBuf {
    let profile = build
        .profile
        .as_deref()
        .or(default_profile)
        .unwrap_or("dev");
    let profile_directory = if profile == "dev" { "debug" } else { profile };
    metadata
        .target_directory
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

fn scaffold(root: &Path, bundle: bool) -> Result {
    let canonical = if root == Path::new(".") {
        std::env::current_dir()?
    } else {
        root.to_path_buf()
    };
    if root != Path::new(".") && root.exists() {
        bail!("{} already exists", root.display());
    }
    if root == Path::new(".") && root.join("Cargo.toml").exists() {
        bail!("Cargo.toml already exists in current directory");
    }
    let package_name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .context("project path must end in valid UTF-8")?;
    validate_project_name(package_name)?;
    let version = env!("CARGO_PKG_VERSION");
    let artifact_name = pascal_name(package_name);
    let display_name = display_name(package_name);
    let excel_prefix = package_name.replace('-', "_").to_ascii_uppercase();
    fs::create_dir_all(root.join("src"))?;
    let bundle_metadata = if bundle {
        "\n[package.metadata.xlfn.bundle]\nx86 = []\nx64 = []\nexternal-imports = []\nstrict-paths = true\n"
    } else {
        ""
    };
    fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname = {package_name:?}\nversion = \"0.1.0\"\nedition = \"2024\"\nrust-version = \"1.97.1\"\n\n[lib]\ncrate-type = [\"cdylib\"]\n\n[dependencies]\nxlfn = {{ version = {version:?} }}\n\n[package.metadata.xlfn]\nartifact-name = {artifact_name:?}\n{bundle_metadata}"
        ),
    )?;
    let struct_name = format!("{artifact_name}Addin");
    fs::write(
        root.join("src/lib.rs"),
        format!(
            "#![deny(unsafe_op_in_unsafe_fn)]\nuse xlfn::prelude::*;\nmod udf;\n\npub struct State;\n\n#[excel_addin(name = {display_name:?}, id = {package_name:?}, category = {artifact_name:?})]\npub struct {struct_name};\n\nimpl Addin for {struct_name} {{\n    type State = State;\n    type Error = XllError;\n\n    fn open(_: &OpenContext) -> Result<State, XllError> {{\n        xlfn::diagnostics::install_file_diagnostic_sink({package_name:?})\n            .map_err(|_| XllError::Internal {{ diagnostic_id: 0x4449_4147_5349_4e4b }})?;\n        Ok(State)\n    }}\n}}\n"
        ),
    )?;
    fs::write(
        root.join("src/udf.rs"),
        format!(
            "use xlfn::prelude::*;\n/// Returns the Add-in version.\n#[excel_function(name = \"{excel_prefix}.VERSION\", thread_safe)]\npub fn version() -> XllResult<String> {{ Ok(env!(\"CARGO_PKG_VERSION\").to_owned()) }}\n"
        ),
    )?;
    if bundle {
        fs::create_dir_all(root.join("vendor/x86"))?;
        fs::create_dir_all(root.join("vendor/x64"))?;
    }
    println!("created {}", root.display());
    Ok(())
}

fn validate_project_name(name: &str) -> Result {
    let valid = !name.is_empty()
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(anyhow!(
            "project name must start with an ASCII letter and contain only letters, digits, '-' or '_'"
        ))
    }
}

fn pascal_name(name: &str) -> String {
    name.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(capitalize)
        .collect()
}

fn display_name(name: &str) -> String {
    name.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(capitalize)
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalize(part: &str) -> String {
    let mut chars = part.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_ascii_uppercase().to_string() + chars.as_str()
    })
}

struct ProjectMetadata {
    package_name: String,
    package_version: String,
    lib_name: String,
    artifact_name: String,
    manifest_path: PathBuf,
    manifest_directory: PathBuf,
    target_directory: PathBuf,
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
        target_directory: cargo.target_directory.as_std_path().to_path_buf(),
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

fn distribute(args: &DistArgs) -> Result {
    let targets = if args.all {
        vec![WindowsTarget::X86, WindowsTarget::X64]
    } else {
        vec![
            args.target
                .context("dist requires --target TARGET or --all")?,
        ]
    };
    let metadata = project_metadata(&args.project, &args.build)?;
    if args.all {
        validate_atomic_output_root(&args.out)?;
        let output_parent = args
            .out
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(output_parent)?;
        let staging_guard = tempfile::Builder::new()
            .prefix(".cargo-xlfn-dist-all-")
            .tempdir_in(output_parent)?;
        let staging_root = staging_guard.path().join("distribution");
        fs::create_dir(&staging_root)?;
        for target in targets {
            stage_distribution_target(
                target,
                args,
                &metadata,
                &staging_root.join(target.directory()),
            )?;
        }
        commit_staged_directory(&staging_root, &args.out)?;
        println!("created {}", args.out.display());
    } else {
        fs::create_dir_all(&args.out)?;
        let target = targets[0];
        let destination = args.out.join(target.directory());
        let staging_guard = tempfile::Builder::new()
            .prefix(&format!(".{}.tmp-", target.directory()))
            .tempdir_in(&args.out)?;
        let staging = staging_guard.path().join("distribution");
        stage_distribution_target(target, args, &metadata, &staging)?;
        commit_staged_directory(&staging, &destination)?;
        println!("created {}", destination.display());
    }
    Ok(())
}

fn validate_atomic_output_root(destination: &Path) -> Result {
    if !matches!(
        destination.components().next_back(),
        Some(Component::Normal(_))
    ) {
        bail!(
            "dist --all output must name a dedicated directory, not {}",
            destination.display()
        );
    }
    if destination.exists() {
        let current = fs::canonicalize(std::env::current_dir()?)?;
        let destination = fs::canonicalize(destination)?;
        if current.starts_with(&destination) {
            bail!(
                "dist --all refuses to replace the current directory or one of its ancestors: {}",
                destination.display()
            );
        }
    }
    Ok(())
}

fn stage_distribution_target(
    target: WindowsTarget,
    args: &DistArgs,
    metadata: &ProjectMetadata,
    staging: &Path,
) -> Result {
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
    args.build.apply_to_command(&mut command, Some("release"));
    if !command.status()?.success() {
        bail!("cargo build failed for {}", target.triple());
    }
    let source = built_library_path(metadata, target.triple(), &args.build, Some("release"));
    if !source.is_file() {
        bail!("built XLL DLL was not found at {}", source.display());
    }
    let bundle = match &metadata.bundle {
        Some(bundle_metadata) => xlfn_package::resolve_bundle_files_with_metadata(
            &metadata.manifest_directory,
            target.triple(),
            bundle_metadata,
        )?,
        None => xlfn_package::ResolvedBundle::empty(),
    };
    validate_bundle_output_names(&bundle, &metadata.artifact_name)?;
    let bundle_sources = bundle
        .resolved_files()
        .map(|(configured_path, resolved_source)| {
            json!({
                "configured_path": configured_path,
                "resolved_source": resolved_source.to_string_lossy(),
            })
        })
        .collect::<Vec<_>>();
    let external_imports = bundle.external_imports().collect::<Vec<_>>();

    let staged_bundle = xlfn_package::stage_bundle(&bundle, staging)?;
    let xll = staging.join(format!("{}.xll", metadata.artifact_name));
    if fs::symlink_metadata(&xll).is_ok() {
        bail!(
            "bundle basename collides with generated XLL: {}",
            xll.display()
        );
    }
    fs::copy(&source, &xll)?;

    // Inspect only the isolated files. These same staged bytes are hashed
    // below and become the committed distribution directory.
    let verified = xlfn_package::verify_staged_package(&xll, target.triple(), &[], staged_bundle)?;

    let files = verified
        .files()
        .iter()
        .map(|path| -> Result<_> {
            Ok(json!({
                "relative_path": path.strip_prefix(staging)?.to_string_lossy(),
                "size": fs::metadata(path)?.len(),
                "sha256": xlfn_package::sha256(path)?,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let manifest = json!({
        "schema": 4,
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
        "bundle_sources": bundle_sources,
        "system_import_policy": {
            "version": xlfn_package::SYSTEM_IMPORT_POLICY_VERSION,
            "external_imports": external_imports,
        },
        "integrity": {
            "purpose": "audit-metadata-only",
            "runtime_verified": false,
            "trust_boundary": "protected-install-location-and-native-code-signing",
        },
        "files": files,
    });
    fs::write(
        staging.join("build-manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

trait DistributionFileOps {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;
}

struct SystemDistributionFileOps;

impl DistributionFileOps for SystemDistributionFileOps {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir_all(path)
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

fn commit_staged_directory(staging: &Path, destination: &Path) -> Result {
    commit_staged_directory_with(staging, destination, &SystemDistributionFileOps)
}

fn commit_staged_directory_with(
    staging: &Path,
    destination: &Path,
    file_ops: &impl DistributionFileOps,
) -> Result {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let destination_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("distribution");
    // The backup lives inside this invocation's private transaction
    // directory. Never claim or remove a predictable user-owned path such as
    // `<destination>.previous`.
    let transaction = tempfile::Builder::new()
        .prefix(&format!(".{destination_name}.transaction-"))
        .tempdir_in(parent)?;
    let previous = transaction.path().join("previous");
    let had_previous = destination.exists();
    if had_previous {
        file_ops.rename(destination, &previous)?;
    }
    if let Err(commit_error) = file_ops.rename(staging, destination) {
        if had_previous && let Err(rollback_error) = file_ops.rename(&previous, destination) {
            let recovery_root = transaction.keep();
            let recovery_path = recovery_root.join("previous");
            return Err(DistributionRecoveryError {
                destination: destination.to_path_buf(),
                commit_error,
                rollback_error,
                recovery_path,
            }
            .into());
        }
        return Err(commit_error.into());
    }
    if had_previous && let Err(error) = file_ops.remove_dir_all(&previous) {
        eprintln!(
            "cargo xlfn: warning: committed {} but could not remove backup {}: {error}",
            destination.display(),
            previous.display()
        );
    }
    Ok(())
}

fn cargo_command() -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    force_static_msvc_crt(&mut command);
    command
}

fn force_static_msvc_crt(command: &mut Command) {
    const CRT_FLAG: &str = "-Ctarget-feature=+crt-static";
    const CRT_CONFIG: &str =
        "target.'cfg(target_env = \"msvc\")'.rustflags=[\"-C\",\"target-feature=+crt-static\"]";

    if let Some(mut flags) = std::env::var_os("CARGO_ENCODED_RUSTFLAGS") {
        if !flags.is_empty() {
            flags.push("\u{1f}");
        }
        flags.push(CRT_FLAG);
        command.env("CARGO_ENCODED_RUSTFLAGS", flags);
    } else if let Some(mut flags) = std::env::var_os("RUSTFLAGS") {
        if !flags.is_empty() {
            flags.push(" ");
        }
        flags.push(CRT_FLAG);
        command.env("RUSTFLAGS", flags);
    } else {
        command.args(["--config", CRT_CONFIG]);
    }
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
        rename_count: Cell<usize>,
        operations: RefCell<Vec<FileOperation>>,
    }

    impl InjectedFileOps {
        fn failing_renames(calls: impl IntoIterator<Item = usize>) -> Self {
            Self {
                failed_renames: calls.into_iter().collect(),
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
            self.operations
                .borrow_mut()
                .push(FileOperation::RemoveDirectory(path.to_path_buf()));
            fs::remove_dir_all(path)
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
        assert!(
            Cli::try_parse_from(["cargo-xlfn", "dist", "--target", "x86_64-pc-window-msvc"])
                .is_err()
        );
    }

    #[test]
    fn default_scaffold_is_one_crate_without_build_script() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("demo");
        scaffold(&root, false).unwrap();
        let cargo = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let library = fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert!(cargo.contains(&format!(
            "xlfn = {{ version = {:?}",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(!cargo.contains("build-dependencies"));
        assert!(!root.join("build.rs").exists());
        assert!(!root.join("src/addin.rs").exists());
        assert!(!library.contains("mod dynamic"));
        assert!(library.contains("#[excel_addin("));
        assert!(!library.contains("xlfn::export!("));
    }

    #[test]
    fn bundle_scaffold_uses_generic_bundle_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("bundle-demo");
        scaffold(&root, true).unwrap();
        let cargo = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("[package.metadata.xlfn.bundle]"));
        assert!(cargo.contains("x86 = []"));
        assert!(!cargo.contains("features = [\"dynamic\"]"));
        assert!(!root.join("src/dynamic.rs").exists());
        assert!(root.join("vendor/x86").is_dir());
        assert!(root.join("vendor/x64").is_dir());
        assert!(!root.join("build.rs").exists());
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

        commit_staged_directory(&staging, &destination).unwrap();

        assert_eq!(fs::read(destination.join("new.xll")).unwrap(), b"new");
        assert!(!destination.join("old.xll").exists());
        assert_eq!(fs::read(unrelated.join("sentinel.txt")).unwrap(), b"keep");
    }

    #[test]
    fn distribution_commit_removes_backup_only_after_installing_staging() {
        let (directory, destination, staging) = distribution_fixture();
        let file_ops = InjectedFileOps::default();

        commit_staged_directory_with(&staging, &destination, &file_ops).unwrap();

        assert_eq!(fs::read(destination.join("new.xll")).unwrap(), b"new");
        assert!(!destination.join("old.xll").exists());
        assert!(transaction_directories(directory.path()).is_empty());
        let operations = file_ops.operations.borrow();
        assert_eq!(operations.len(), 3);
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
    }

    #[test]
    fn distribution_commit_failure_rolls_previous_distribution_back() {
        let (directory, destination, staging) = distribution_fixture();
        let file_ops = InjectedFileOps::failing_renames([2]);

        let error = commit_staged_directory_with(&staging, &destination, &file_ops).unwrap_err();

        assert!(error.to_string().contains("injected rename failure #2"));
        assert_eq!(fs::read(destination.join("old.xll")).unwrap(), b"old");
        assert_eq!(fs::read(staging.join("new.xll")).unwrap(), b"new");
        assert!(transaction_directories(directory.path()).is_empty());
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
    fn distribution_preserves_recovery_path_when_commit_and_rollback_fail() {
        let (directory, destination, staging) = distribution_fixture();
        let file_ops = InjectedFileOps::failing_renames([2, 3]);

        let error = commit_staged_directory_with(&staging, &destination, &file_ops).unwrap_err();
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
    fn atomic_distribution_requires_a_dedicated_output_directory() {
        assert!(validate_atomic_output_root(Path::new(".")).is_err());
        assert!(validate_atomic_output_root(Path::new("..")).is_err());
        assert!(validate_atomic_output_root(Path::new("/")).is_err());
        assert!(validate_atomic_output_root(Path::new("dist")).is_ok());
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
            "--locked",
        ])
        .unwrap();
        let Commands::Check(args) = parsed.command else {
            panic!("expected Check command");
        };
        assert_eq!(args.build.profile.as_deref(), Some("release"));
        assert_eq!(args.build.features, vec!["feat1", "feat2"]);
        assert!(args.build.locked);
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
    fn packaging_build_forces_a_static_msvc_crt() {
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
        assert!(
            arguments
                .iter()
                .chain(&environment)
                .any(|value| value.to_string_lossy().contains("+crt-static"))
        );
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
