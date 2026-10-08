use super::*;
use cargo_metadata::Message;
use std::io::{BufRead, BufReader};
use std::process::Stdio;

/// Observations from the selected library's actual Cargo build. Workspace
/// metadata may unify features with members not selected by `cargo build -p`.
pub(crate) struct BuiltLibrary {
    pub(crate) path: PathBuf,
    pub(crate) features: Vec<String>,
}

pub(crate) fn run_cargo_build(
    command: &mut Command,
    metadata: &ProjectMetadata,
    library_extension: &str,
) -> Result<BuiltLibrary> {
    command
        .arg("--message-format=json-render-diagnostics")
        .stdout(Stdio::piped());
    let mut child = command.spawn().context("could not start cargo build")?;
    let output = read_build_messages(
        BufReader::new(child.stdout.take().expect("piped Cargo stdout")),
        metadata,
        library_extension,
    );
    if output.is_err() {
        // A failed pipe read must not leave a child blocked on an unread pipe.
        let _ = child.kill();
    }
    let status = child.wait().context("could not wait for cargo build")?;
    let built = output?;
    if !status.success() {
        bail!("cargo build failed for {}", metadata.package_name);
    }
    Ok(built)
}

fn read_build_messages(
    reader: impl BufRead,
    metadata: &ProjectMetadata,
    library_extension: &str,
) -> Result<BuiltLibrary> {
    let mut libraries = Vec::new();
    let mut build_succeeded = false;
    for message in Message::parse_stream(reader) {
        match message.context("could not read cargo build output")? {
            Message::CompilerArtifact(artifact)
                if artifact.package_id == metadata.package_id
                    && artifact.target.name == metadata.lib_name
                    && artifact
                        .target
                        .kind
                        .iter()
                        .any(|kind| kind.to_string() == "cdylib") =>
            {
                for path in artifact.filenames {
                    if path.extension() == Some(library_extension) {
                        libraries.push(BuiltLibrary {
                            path: path.into_std_path_buf(),
                            features: artifact.features.clone(),
                        });
                    }
                }
            }
            Message::CompilerMessage(message) => {
                if let Some(rendered) = message.message.rendered {
                    eprint!("{rendered}");
                } else {
                    eprintln!("{}", message.message.message);
                }
            }
            Message::BuildFinished(finished) => build_succeeded = finished.success,
            Message::TextLine(line) => println!("{line}"),
            _ => {}
        }
    }
    if !build_succeeded {
        bail!("cargo build did not report successful completion");
    }
    if libraries.len() != 1 {
        bail!(
            "cargo build reported {} matching cdylib artifacts for {}, expected exactly one",
            libraries.len(),
            metadata.package_name
        );
    }
    let mut built = libraries.pop().expect("exactly one library artifact");
    built.features.sort_unstable();
    built.features.dedup();
    Ok(built)
}

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
    crt::configure_wrapper(
        command,
        metadata.crt,
        target,
        metadata.rustc_wrapper.as_deref(),
    )
}

#[cfg(test)]
mod build_output_tests;
