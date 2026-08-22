use anyhow::{Context, Result, anyhow, bail};
use clap::ValueEnum;
use serde_json::{Value, json};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use xlfn_package::{EffectiveCrtPolicy, PeInfo};

const WRAPPER_MODE: &str = "XLFN_RUSTC_WRAPPER_MODE";
const WRAPPER_POLICY: &str = "XLFN_CRT_POLICY";
const WRAPPER_TARGET: &str = "XLFN_CRT_TARGET";
const UPSTREAM_WRAPPER: &str = "XLFN_UPSTREAM_RUSTC_WRAPPER";

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum CrtPolicy {
    /// Do not alter Cargo or rustc CRT settings.
    Inherit,
    /// Compile target Rust crates against the static MSVC CRT.
    Static,
    /// Compile target Rust crates against the dynamic MSVC CRT.
    Dynamic,
}

impl CrtPolicy {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Static => "static",
            Self::Dynamic => "dynamic",
        }
    }

    pub(crate) fn parse_metadata(value: &Value) -> Result<Self> {
        let value = value
            .as_str()
            .context("[package.metadata.xlfn].crt must be a string")?;
        match value {
            "inherit" => Ok(Self::Inherit),
            "static" => Ok(Self::Static),
            "dynamic" => Ok(Self::Dynamic),
            _ => bail!(
                "[package.metadata.xlfn].crt must be one of inherit, static, or dynamic, got {value:?}"
            ),
        }
    }
}

impl fmt::Display for CrtPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CrtPolicySource {
    Cli,
    PackageMetadata,
    Default,
}

impl CrtPolicySource {
    const fn name(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::PackageMetadata => "package-metadata",
            Self::Default => "default",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedCrtPolicy {
    pub(crate) policy: CrtPolicy,
    pub(crate) source: CrtPolicySource,
}

impl ResolvedCrtPolicy {
    pub(crate) fn resolve(cli: Option<CrtPolicy>, metadata: Option<CrtPolicy>) -> Self {
        if let Some(policy) = cli {
            return Self {
                policy,
                source: CrtPolicySource::Cli,
            };
        }
        if let Some(policy) = metadata {
            return Self {
                policy,
                source: CrtPolicySource::PackageMetadata,
            };
        }
        Self {
            policy: CrtPolicy::Static,
            source: CrtPolicySource::Default,
        }
    }

    pub(crate) fn print(self) {
        println!("CRT policy: {} ({})", self.policy, self.source.name());
        if self.source == CrtPolicySource::Default {
            println!("Use --crt inherit or --crt dynamic to override.");
        }
    }

    pub(crate) fn target_directory(self, base: &Path) -> PathBuf {
        base.join(format!("xlfn-crt-{}", self.policy.name()))
    }

    pub(crate) fn enforcement(self) -> &'static str {
        match self.policy {
            CrtPolicy::Inherit => "inherited",
            CrtPolicy::Static | CrtPolicy::Dynamic => "rustc-wrapper",
        }
    }
}

pub(crate) fn configure_wrapper(
    command: &mut Command,
    resolved: ResolvedCrtPolicy,
    target: &str,
) -> Result<()> {
    if !target.ends_with("-pc-windows-msvc") {
        return Ok(());
    }
    validate_explicit_policy_target(resolved.policy, target)?;
    let current_executable = std::env::current_exe()
        .context("failed to locate cargo-xlfn for the internal rustc wrapper")?;
    if let Some(upstream) = std::env::var_os("RUSTC_WRAPPER") {
        command.env(UPSTREAM_WRAPPER, upstream);
    } else {
        command.env_remove(UPSTREAM_WRAPPER);
    }
    command.env("RUSTC_WRAPPER", current_executable);
    command.env(WRAPPER_MODE, "1");
    command.env(WRAPPER_POLICY, resolved.policy.name());
    command.env(WRAPPER_TARGET, target);
    Ok(())
}

pub(crate) fn validate_explicit_policy_target(policy: CrtPolicy, target: &str) -> Result<()> {
    if policy != CrtPolicy::Inherit && !target.ends_with("-pc-windows-msvc") {
        bail!("--crt {policy} is only supported for MSVC targets, got {target}");
    }
    Ok(())
}

pub(crate) fn wrapper_mode_requested() -> bool {
    std::env::var_os(WRAPPER_MODE).is_some()
}

pub(crate) fn run_wrapper() -> Result<ExitStatus> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let target = std::env::var_os(WRAPPER_TARGET)
        .context("internal rustc wrapper is missing its selected target")?;
    let policy_value =
        std::env::var(WRAPPER_POLICY).context("internal rustc wrapper is missing CRT policy")?;
    let policy = match policy_value.as_str() {
        "static" => CrtPolicy::Static,
        "dynamic" => CrtPolicy::Dynamic,
        "inherit" => CrtPolicy::Inherit,
        value => bail!("internal rustc wrapper has invalid CRT policy {value:?}"),
    };
    let args = wrapper_arguments(args, policy, &target);
    let upstream = std::env::var_os(UPSTREAM_WRAPPER);
    let mut command = compiler_chain(&args, upstream.as_deref())?;
    command
        .env_remove(WRAPPER_MODE)
        .env_remove(WRAPPER_POLICY)
        .env_remove(WRAPPER_TARGET)
        .env_remove(UPSTREAM_WRAPPER);
    command.status().context("internal rustc wrapper failed")
}

fn compiler_chain(args: &[OsString], upstream: Option<&OsStr>) -> Result<Command> {
    if let Some(upstream) = upstream {
        let mut command = Command::new(upstream);
        command.args(args);
        return Ok(command);
    }
    let (compiler, compiler_args) = args
        .split_first()
        .ok_or_else(|| anyhow!("internal rustc wrapper received no compiler path"))?;
    let mut command = Command::new(compiler);
    command.args(compiler_args);
    Ok(command)
}

fn wrapper_arguments(
    mut args: Vec<OsString>,
    policy: CrtPolicy,
    selected_target: &OsStr,
) -> Vec<OsString> {
    if rustc_invocation_targets(&args, selected_target) {
        match policy {
            CrtPolicy::Static => {
                args.push(OsString::from("-C"));
                args.push(OsString::from("target-feature=+crt-static"));
            }
            CrtPolicy::Dynamic => {
                args.push(OsString::from("-C"));
                args.push(OsString::from("target-feature=-crt-static"));
            }
            CrtPolicy::Inherit => {}
        }
        if is_cdylib_link(&args) {
            args.push(OsString::from("-C"));
            args.push(OsString::from("link-arg=/IGNORE:4104"));
        }
    }
    args
}

fn is_cdylib_link(args: &[OsString]) -> bool {
    args.array_windows::<2>()
        .any(|[flag, value]| flag == "--crate-type" && value == "cdylib")
        || args.iter().any(|argument| {
            argument
                .to_str()
                .and_then(|argument| argument.strip_prefix("--crate-type="))
                .is_some_and(|crate_type| crate_type.split(',').any(|t| t == "cdylib"))
        })
}

fn rustc_invocation_targets(args: &[OsString], selected_target: &OsStr) -> bool {
    args.array_windows::<2>()
        .any(|[flag, value]| flag == "--target" && value == selected_target)
        || args.iter().any(|argument| {
            argument
                .to_str()
                .and_then(|argument| argument.strip_prefix("--target="))
                .is_some_and(|target| target == selected_target)
        })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CrtObservation {
    pub(crate) effective_rust: EffectiveCrtPolicy,
    pub(crate) observed_dynamic_crt_imports: Vec<String>,
    pub(crate) consistency: &'static str,
}

impl CrtObservation {
    pub(crate) fn inspect(info: &PeInfo, requested: ResolvedCrtPolicy) -> Result<Self> {
        let effective_rust = info
            .crt_policy
            .context("generated XLL is missing its .xlfncrt effective CRT marker")?;
        let mut observed_dynamic_crt_imports = info
            .imports
            .iter()
            .chain(&info.delay_imports)
            .filter(|name| is_dynamic_crt_import(name))
            .cloned()
            .collect::<Vec<_>>();
        observed_dynamic_crt_imports.sort_by_key(|name| name.to_ascii_lowercase());
        observed_dynamic_crt_imports.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

        match requested.policy {
            CrtPolicy::Static if effective_rust != EffectiveCrtPolicy::Static => bail!(
                "--crt static was requested, but the generated XLL reports dynamic Rust CRT linkage"
            ),
            CrtPolicy::Dynamic if effective_rust != EffectiveCrtPolicy::Dynamic => bail!(
                "--crt dynamic was requested, but the generated XLL reports static Rust CRT linkage"
            ),
            _ => {}
        }
        if requested.policy == CrtPolicy::Static && !observed_dynamic_crt_imports.is_empty() {
            bail!(
                "--crt static was requested, but the generated XLL imports dynamic MSVC runtime libraries: {}. A linked native static library may have been compiled with /MD; rebuild it with /MT or use --crt dynamic",
                observed_dynamic_crt_imports.join(", ")
            );
        }
        let consistency = if requested.policy == CrtPolicy::Inherit
            && effective_rust == EffectiveCrtPolicy::Static
            && !observed_dynamic_crt_imports.is_empty()
        {
            "potentially-mixed"
        } else {
            "verified"
        };
        Ok(Self {
            effective_rust,
            observed_dynamic_crt_imports,
            consistency,
        })
    }

    pub(crate) fn warn_if_mixed(&self) {
        if self.consistency == "potentially-mixed" {
            eprintln!(
                "cargo xlfn: warning: Rust CRT policy is static but the XLL imports dynamic CRT libraries: {}",
                self.observed_dynamic_crt_imports.join(", ")
            );
        }
    }

    pub(crate) fn manifest(self, requested: ResolvedCrtPolicy) -> Value {
        json!({
            "requested": requested.policy.name(),
            "source": requested.source.name(),
            "effective_rust": self.effective_rust.name(),
            "enforcement": requested.enforcement(),
            "observed_dynamic_crt_imports": self.observed_dynamic_crt_imports,
            "consistency": self.consistency,
        })
    }
}

fn is_dynamic_crt_import(name: &str) -> bool {
    const DYNAMIC_CRT_IMPORTS: &[&str] = &[
        "ucrtbase.dll",
        "ucrtbased.dll",
        "vcruntime140.dll",
        "vcruntime140_1.dll",
        "msvcp140.dll",
        "msvcp140_1.dll",
        "msvcp140_2.dll",
        "msvcp140_atomic_wait.dll",
        "msvcp140_codecvt_ids.dll",
        "api-ms-win-crt-conio-l1-1-0.dll",
        "api-ms-win-crt-convert-l1-1-0.dll",
        "api-ms-win-crt-environment-l1-1-0.dll",
        "api-ms-win-crt-filesystem-l1-1-0.dll",
        "api-ms-win-crt-heap-l1-1-0.dll",
        "api-ms-win-crt-locale-l1-1-0.dll",
        "api-ms-win-crt-math-l1-1-0.dll",
        "api-ms-win-crt-multibyte-l1-1-0.dll",
        "api-ms-win-crt-private-l1-1-0.dll",
        "api-ms-win-crt-process-l1-1-0.dll",
        "api-ms-win-crt-runtime-l1-1-0.dll",
        "api-ms-win-crt-stdio-l1-1-0.dll",
        "api-ms-win-crt-string-l1-1-0.dll",
        "api-ms-win-crt-time-l1-1-0.dll",
        "api-ms-win-crt-utility-l1-1-0.dll",
    ];
    DYNAMIC_CRT_IMPORTS
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_resolution_has_a_visible_static_default() {
        let resolved = ResolvedCrtPolicy::resolve(None, None);
        assert_eq!(resolved.policy, CrtPolicy::Static);
        assert_eq!(resolved.source, CrtPolicySource::Default);
    }

    #[test]
    fn cli_policy_overrides_package_metadata() {
        let resolved =
            ResolvedCrtPolicy::resolve(Some(CrtPolicy::Dynamic), Some(CrtPolicy::Static));
        assert_eq!(resolved.policy, CrtPolicy::Dynamic);
        assert_eq!(resolved.source, CrtPolicySource::Cli);
    }

    #[test]
    fn non_msvc_target_is_a_wrapper_noop() {
        let mut command = Command::new("cargo");
        configure_wrapper(
            &mut command,
            ResolvedCrtPolicy::resolve(Some(CrtPolicy::Inherit), None),
            "x86_64-unknown-linux-gnu",
        )
        .unwrap();
        assert!(command.get_envs().next().is_none());
    }

    #[test]
    fn cdylib_link_suppresses_com_private_export_warning() {
        let input = vec![
            "rustc".into(),
            "--target".into(),
            "x86_64-pc-windows-msvc".into(),
            "--crate-type".into(),
            "cdylib".into(),
        ];
        let output = wrapper_arguments(
            input,
            CrtPolicy::Inherit,
            OsStr::new("x86_64-pc-windows-msvc"),
        );
        assert!(output.iter().any(|arg| arg == "link-arg=/IGNORE:4104"));
        assert!(
            !output
                .iter()
                .any(|arg| arg.to_string_lossy().starts_with("link-arg=/DEF:"))
        );
    }

    #[test]
    fn wrapper_changes_only_matching_target_invocations() {
        let input = vec![
            "rustc".into(),
            "--target".into(),
            "x86_64-pc-windows-msvc".into(),
            "-C".into(),
            "target-feature=+avx2".into(),
        ];
        let output = wrapper_arguments(
            input,
            CrtPolicy::Dynamic,
            OsStr::new("x86_64-pc-windows-msvc"),
        );
        assert!(output.iter().any(|arg| arg == "target-feature=+avx2"));
        assert!(output.iter().any(|arg| arg == "target-feature=-crt-static"));

        let host = wrapper_arguments(
            vec!["rustc".into(), "--crate-name".into(), "build_script".into()],
            CrtPolicy::Static,
            OsStr::new("x86_64-pc-windows-msvc"),
        );
        assert!(!host.iter().any(|arg| arg == "target-feature=+crt-static"));
    }

    #[test]
    fn upstream_wrapper_receives_the_original_compiler_chain() {
        let args = vec!["rustc".into(), "--crate-name".into(), "demo".into()];
        let command = compiler_chain(&args, Some(OsStr::new("sccache"))).unwrap();
        assert_eq!(command.get_program(), "sccache");
        assert_eq!(command.get_args().collect::<Vec<_>>(), args);
    }

    #[test]
    fn inherit_reports_potential_static_dynamic_mixing() {
        let mut info = PeInfo {
            crt_policy: Some(EffectiveCrtPolicy::Static),
            ..PeInfo::default()
        };
        info.imports.insert("UCRTBASE.dll".to_owned());
        let observation = CrtObservation::inspect(
            &info,
            ResolvedCrtPolicy::resolve(Some(CrtPolicy::Inherit), None),
        )
        .unwrap();
        assert_eq!(observation.consistency, "potentially-mixed");
        assert_eq!(observation.observed_dynamic_crt_imports, ["UCRTBASE.dll"]);
    }

    #[test]
    fn static_rejects_direct_dynamic_crt_imports() {
        let mut info = PeInfo {
            crt_policy: Some(EffectiveCrtPolicy::Static),
            ..PeInfo::default()
        };
        info.imports.insert("vcruntime140.dll".to_owned());
        assert!(
            CrtObservation::inspect(
                &info,
                ResolvedCrtPolicy::resolve(Some(CrtPolicy::Static), None),
            )
            .is_err()
        );
    }

    #[test]
    fn explicit_policy_rejects_non_msvc_targets() {
        assert!(
            validate_explicit_policy_target(CrtPolicy::Static, "x86_64-unknown-linux-gnu").is_err()
        );
        assert!(
            validate_explicit_policy_target(CrtPolicy::Inherit, "x86_64-unknown-linux-gnu").is_ok()
        );
    }

    #[test]
    fn dynamic_crt_import_names_are_classified_case_insensitively() {
        assert!(is_dynamic_crt_import("UCRTBASE.DLL"));
        assert!(is_dynamic_crt_import("vcruntime140_1.dll"));
        assert!(is_dynamic_crt_import("api-ms-win-crt-heap-l1-1-0.dll"));
        assert!(!is_dynamic_crt_import("kernel32.dll"));
        assert!(!is_dynamic_crt_import("msvcp_private.dll"));
        assert!(!is_dynamic_crt_import("vcruntime_payload.dll"));
        assert!(!is_dynamic_crt_import("api-ms-win-crt-untrusted.dll"));
        assert!(is_dynamic_crt_import("ucrtbased.dll"));
    }
}
