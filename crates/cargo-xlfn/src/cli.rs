use super::*;

#[derive(Parser)]
#[command(name = "cargo xlfn", version, about)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    Check(CheckArgs),
    Package(PackageArgs),
}

#[derive(Args)]
pub(crate) struct CheckArgs {
    #[command(flatten)]
    pub(crate) project: ProjectArgs,
    #[command(flatten)]
    pub(crate) build: BuildSelectionArgs,
    #[arg(long, value_enum)]
    pub(crate) target: Option<WindowsTarget>,
}

#[derive(Args, Clone, Debug, Default)]
pub(crate) struct BuildSelectionArgs {
    /// MSVC CRT policy for target Rust crates.
    #[arg(long, value_enum)]
    pub(crate) crt: Option<CrtPolicy>,
    /// Base Cargo target directory; the CRT policy is appended to this path.
    #[arg(long)]
    pub(crate) target_dir: Option<PathBuf>,
    #[arg(long)]
    pub(crate) profile: Option<String>,
    #[arg(long, value_delimiter = ',')]
    pub(crate) features: Vec<String>,
    #[arg(long)]
    pub(crate) no_default_features: bool,
    #[arg(long)]
    pub(crate) all_features: bool,
    #[arg(long)]
    pub(crate) locked: bool,
    #[arg(long)]
    pub(crate) frozen: bool,
    #[arg(long)]
    pub(crate) offline: bool,
}

impl BuildSelectionArgs {
    pub(crate) fn apply_to_command(&self, command: &mut Command, default_profile: Option<&str>) {
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

    pub(crate) fn apply_to_metadata(&self, command: &mut MetadataCommand) {
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

    pub(crate) fn apply_resolution_constraints(&self, command: &mut MetadataCommand) {
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
pub(crate) struct ProjectArgs {
    #[arg(long)]
    pub(crate) manifest_path: Option<PathBuf>,
    #[arg(long)]
    pub(crate) package: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum WindowsTarget {
    #[value(name = "i686-pc-windows-msvc")]
    X86,
    #[value(name = "x86_64-pc-windows-msvc")]
    X64,
}

impl WindowsTarget {
    pub(crate) const fn triple(self) -> &'static str {
        match self {
            Self::X86 => "i686-pc-windows-msvc",
            Self::X64 => "x86_64-pc-windows-msvc",
        }
    }

    pub(crate) const fn directory(self) -> &'static str {
        match self {
            Self::X86 => "win-x86",
            Self::X64 => "win-x64",
        }
    }
}

#[derive(Args)]
pub(crate) struct PackageArgs {
    #[arg(long, value_enum, conflicts_with = "all")]
    pub(crate) target: Option<WindowsTarget>,
    #[arg(long, conflicts_with = "target")]
    pub(crate) all: bool,
    #[arg(long, default_value = "package")]
    pub(crate) out: PathBuf,
    #[command(flatten)]
    pub(crate) project: ProjectArgs,
    #[command(flatten)]
    pub(crate) build: BuildSelectionArgs,
}

pub(crate) fn normalize_cargo_subcommand_args(
    mut args: Vec<std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    if args.get(1).is_some_and(|arg| arg == "xlfn") {
        args.remove(1);
    }
    args
}

pub(crate) fn run() -> Result {
    let args = normalize_cargo_subcommand_args(std::env::args_os().collect());
    match Cli::parse_from(args).command {
        Commands::Check(args) => check(&args),
        Commands::Package(args) => package(&args),
    }
}
