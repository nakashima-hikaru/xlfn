use super::*;

#[derive(Cli)]
#[usage(bin = "cargo xlfn", version, about)]
pub(crate) struct Cli {
    #[usage(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommands)]
pub(crate) enum Commands {
    Check(CheckArgs),
    Package(PackageArgs),
}

#[derive(Args, Clone, Debug, Default)]
pub(crate) struct CheckArgs {
    #[usage(long)]
    pub(crate) manifest_path: Option<PathBuf>,
    #[usage(long)]
    pub(crate) package: Option<String>,
    /// MSVC CRT policy for target Rust crates.
    #[usage(long)]
    pub(crate) crt: Option<CrtPolicy>,
    /// Base Cargo target directory; the CRT policy is appended to this path.
    #[usage(long)]
    pub(crate) target_dir: Option<PathBuf>,
    #[usage(long)]
    pub(crate) profile: Option<String>,
    #[usage(long)]
    pub(crate) features: Vec<String>,
    #[usage(long)]
    pub(crate) no_default_features: bool,
    #[usage(long)]
    pub(crate) all_features: bool,
    #[usage(long)]
    pub(crate) locked: bool,
    #[usage(long)]
    pub(crate) frozen: bool,
    #[usage(long)]
    pub(crate) offline: bool,
    #[usage(long)]
    pub(crate) target: Option<WindowsTarget>,
}

impl CheckArgs {
    pub(crate) fn project(&self) -> ProjectArgs {
        ProjectArgs {
            manifest_path: self.manifest_path.clone(),
            package: self.package.clone(),
        }
    }

    pub(crate) fn build(&self) -> BuildSelectionArgs {
        BuildSelectionArgs {
            crt: self.crt,
            target_dir: self.target_dir.clone(),
            profile: self.profile.clone(),
            features: self.features.clone(),
            no_default_features: self.no_default_features,
            all_features: self.all_features,
            locked: self.locked,
            frozen: self.frozen,
            offline: self.offline,
        }
    }
}

#[derive(Args, Clone, Debug, Default)]
pub(crate) struct BuildSelectionArgs {
    /// MSVC CRT policy for target Rust crates.
    #[usage(long)]
    pub(crate) crt: Option<CrtPolicy>,
    /// Base Cargo target directory; the CRT policy is appended to this path.
    #[usage(long)]
    pub(crate) target_dir: Option<PathBuf>,
    #[usage(long)]
    pub(crate) profile: Option<String>,
    #[usage(long)]
    pub(crate) features: Vec<String>,
    #[usage(long)]
    pub(crate) no_default_features: bool,
    #[usage(long)]
    pub(crate) all_features: bool,
    #[usage(long)]
    pub(crate) locked: bool,
    #[usage(long)]
    pub(crate) frozen: bool,
    #[usage(long)]
    pub(crate) offline: bool,
}

impl BuildSelectionArgs {
    pub(crate) fn normalized_features(&self) -> Vec<String> {
        self.features
            .iter()
            .flat_map(|f| f.split(','))
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_owned)
            .collect()
    }

    pub(crate) fn apply_to_command(&self, command: &mut Command, default_profile: Option<&str>) {
        let profile = self.profile.as_deref().or(default_profile);
        if let Some(profile) = profile {
            command.arg("--profile").arg(profile);
        }
        let features = self.normalized_features();
        if !features.is_empty() {
            command.arg("--features").arg(features.join(","));
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
        let features = self.normalized_features();
        if !features.is_empty() {
            command.features(CargoOpt::SomeFeatures(features));
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
    #[usage(long)]
    pub(crate) manifest_path: Option<PathBuf>,
    #[usage(long)]
    pub(crate) package: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum WindowsTarget {
    #[usage(name = "i686-pc-windows-msvc")]
    X86,
    #[usage(name = "x86_64-pc-windows-msvc")]
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

impl std::str::FromStr for WindowsTarget {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "i686-pc-windows-msvc" | "x86" => Ok(Self::X86),
            "x86_64-pc-windows-msvc" | "x64" => Ok(Self::X64),
            _ => bail!(
                "unsupported target {s:?}, expected i686-pc-windows-msvc or x86_64-pc-windows-msvc"
            ),
        }
    }
}

#[derive(Args, Clone, Debug, Default)]
pub(crate) struct PackageArgs {
    #[usage(long, conflicts = ["all"])]
    pub(crate) target: Option<WindowsTarget>,
    #[usage(long, conflicts = ["target"])]
    pub(crate) all: bool,
    #[usage(long, default = "package")]
    pub(crate) out: PathBuf,
    #[usage(long)]
    pub(crate) manifest_path: Option<PathBuf>,
    #[usage(long)]
    pub(crate) package: Option<String>,
    /// MSVC CRT policy for target Rust crates.
    #[usage(long)]
    pub(crate) crt: Option<CrtPolicy>,
    /// Base Cargo target directory; the CRT policy is appended to this path.
    #[usage(long)]
    pub(crate) target_dir: Option<PathBuf>,
    #[usage(long)]
    pub(crate) profile: Option<String>,
    #[usage(long)]
    pub(crate) features: Vec<String>,
    #[usage(long)]
    pub(crate) no_default_features: bool,
    #[usage(long)]
    pub(crate) all_features: bool,
    #[usage(long)]
    pub(crate) locked: bool,
    #[usage(long)]
    pub(crate) frozen: bool,
    #[usage(long)]
    pub(crate) offline: bool,
}

impl PackageArgs {
    pub(crate) fn project(&self) -> ProjectArgs {
        ProjectArgs {
            manifest_path: self.manifest_path.clone(),
            package: self.package.clone(),
        }
    }

    pub(crate) fn build(&self) -> BuildSelectionArgs {
        BuildSelectionArgs {
            crt: self.crt,
            target_dir: self.target_dir.clone(),
            profile: self.profile.clone(),
            features: self.features.clone(),
            no_default_features: self.no_default_features,
            all_features: self.all_features,
            locked: self.locked,
            frozen: self.frozen,
            offline: self.offline,
        }
    }
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
    let argv: Vec<&std::ffi::OsStr> = args.iter().map(std::ops::Deref::deref).collect();
    let cli = match Cli::parse_from_argv(&argv) {
        Ok(cli) => cli,
        Err(usage::Error::Help { cmd, long }) => {
            let page = if long {
                usage::help::Page::Long
            } else {
                usage::help::Page::Short
            };
            if let Some(rendered) = usage::help::page(
                Cli::spec(),
                Cli::command(),
                &argv,
                cmd,
                page,
                usage::help::Style::auto(),
            ) {
                print!("{rendered}");
            }
            std::process::exit(0);
        }
        Err(usage::Error::HelpAll { cmd }) => {
            if let Some(rendered) = usage::help::page(
                Cli::spec(),
                Cli::command(),
                &argv,
                cmd,
                usage::help::Page::All,
                usage::help::Style::auto(),
            ) {
                print!("{rendered}");
            }
            std::process::exit(0);
        }
        Err(usage::Error::Version { .. }) => {
            println!("cargo-xlfn {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
        Err(usage::Error::MissingArgsHelp { cmd }) => {
            if let Some(rendered) = usage::help::page(
                Cli::spec(),
                Cli::command(),
                &argv,
                cmd,
                usage::help::Page::Short,
                usage::help::Style::auto_stderr(),
            ) {
                eprint!("{rendered}");
            }
            std::process::exit(2);
        }
        Err(err) => {
            let rendered = usage::render_failure(Cli::spec(), &argv, &err);
            eprint!("{rendered}");
            std::process::exit(1);
        }
    };
    match cli.command {
        Commands::Check(args) => check(&args),
        Commands::Package(args) => package(&args),
    }
}
