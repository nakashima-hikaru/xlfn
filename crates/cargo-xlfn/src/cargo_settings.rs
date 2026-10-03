//! Resolve the Cargo settings that the CRT wrapper and build provenance need.
//! Cargo discovers configuration from its invocation directory, independently
//! of `--manifest-path`. Keep source-relative paths while merging these keys.

use super::*;
use serde::Deserialize;
use std::ffi::{OsStr, OsString};

#[derive(Default)]
pub(crate) struct CargoSettings {
    pub(crate) rustc_wrapper: Option<PathBuf>,
    lockfile: Option<PathBuf>,
}

impl CargoSettings {
    pub(crate) fn load(cwd: &Path) -> Result<Self> {
        let cargo_home = home::cargo_home_with_cwd(cwd).context("could not locate Cargo home")?;
        Self::load_with(cwd, Some(&cargo_home), |key| std::env::var_os(key))
    }

    fn load_with(
        cwd: &Path,
        cargo_home: Option<&Path>,
        environment: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self> {
        let mut directories = cwd
            .ancestors()
            .map(|directory| directory.join(".cargo"))
            .collect::<Vec<_>>();
        let canonical_home = cargo_home.and_then(|path| fs::canonicalize(path).ok());
        if let Some(cargo_home) = cargo_home
            && !directories.iter().any(|directory| {
                directory == cargo_home
                    || canonical_home.as_ref().is_some_and(|home| {
                        fs::canonicalize(directory).is_ok_and(|directory| &directory == home)
                    })
            })
        {
            directories.push(cargo_home.to_path_buf());
        }
        let mut settings = Self::default();
        for directory in directories.into_iter().rev() {
            // Cargo prefers the extensionless file when both forms exist.
            let legacy = directory.join("config");
            let modern = directory.join("config.toml");
            let path = if legacy.try_exists()? {
                legacy
            } else if modern.try_exists()? {
                modern
            } else {
                continue;
            };
            settings.merge(Self::read_file(&path, &mut Vec::new())?);
        }
        let unicode_environment = |key| environment(key).filter(|value| value.to_str().is_some());
        if let Some(wrapper) = unicode_environment("RUSTC_WRAPPER")
            .or_else(|| unicode_environment("CARGO_BUILD_RUSTC_WRAPPER"))
        {
            // An explicitly empty RUSTC_WRAPPER disables configured wrappers.
            settings.rustc_wrapper = Some(program_path(&wrapper, cwd));
        }
        if let Some(lockfile) = unicode_environment("CARGO_RESOLVER_LOCKFILE_PATH") {
            settings.lockfile = Some(cwd.join(lockfile));
        }
        if let Some(lockfile) = &settings.lockfile
            && lockfile.file_name() != Some(OsStr::new("Cargo.lock"))
        {
            bail!("resolver.lockfile-path must end with Cargo.lock, got {lockfile:?}");
        }
        Ok(settings)
    }

    pub(crate) fn lockfile_path(&self, workspace_root: &Path) -> PathBuf {
        self.lockfile
            .clone()
            .unwrap_or_else(|| workspace_root.join("Cargo.lock"))
    }

    fn merge(&mut self, higher: Self) {
        if higher.rustc_wrapper.is_some() {
            self.rustc_wrapper = higher.rustc_wrapper;
        }
        if higher.lockfile.is_some() {
            self.lockfile = higher.lockfile;
        }
    }

    fn read_file(path: &Path, seen: &mut Vec<PathBuf>) -> Result<Self> {
        // Cargo tracks every included lexical path for this top-level file,
        // including repeated siblings. Symlink targets keep distinct origins.
        if seen.iter().any(|previous| previous == path) {
            bail!("Cargo configuration include cycle at {path:?}");
        }
        seen.push(path.to_path_buf());
        let config: ConfigFile = toml::from_str(&fs::read_to_string(path)?)
            .with_context(|| format!("could not parse Cargo configuration {path:?}"))?;
        let directory = path.parent().context("Cargo configuration has no parent")?;
        let root = directory
            .parent()
            .context("Cargo configuration has no path resolution root")?;
        let mut settings = Self::default();
        for include in config.include {
            let (relative, optional) = match include {
                ConfigInclude::Path(path) => (path, false),
                ConfigInclude::Detailed { path, optional } => (path, optional),
            };
            let included = normalize_path(&directory.join(relative));
            if included.extension() != Some(OsStr::new("toml")) {
                bail!("Cargo configuration include must end with .toml: {included:?}");
            }
            if optional && !included.try_exists()? {
                continue;
            }
            settings.merge(Self::read_file(&included, seen)?);
        }
        settings.merge(Self {
            rustc_wrapper: config
                .build
                .rustc_wrapper
                .map(|wrapper| program_path(OsStr::new(&wrapper), root)),
            lockfile: config.resolver.lockfile_path.map(|path| root.join(path)),
        });
        Ok(settings)
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push(component);
                }
            }
            _ => normalized.push(component),
        }
    }
    normalized
}

fn program_path(program: &OsStr, root: &Path) -> PathBuf {
    // Cargo treats either separator as a path, even on Unix. Bare program names
    // (and the empty disabling value) retain PATH lookup semantics.
    if program
        .as_encoded_bytes()
        .iter()
        .any(|byte| matches!(byte, b'/' | b'\\'))
    {
        root.join(program)
    } else {
        PathBuf::from(program)
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ConfigFile {
    include: Vec<ConfigInclude>,
    build: BuildConfig,
    resolver: ResolverConfig,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ConfigInclude {
    Path(String),
    Detailed {
        path: String,
        #[serde(default)]
        optional: bool,
    },
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct BuildConfig {
    rustc_wrapper: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct ResolverConfig {
    lockfile_path: Option<String>,
}

#[cfg(test)]
mod tests;
