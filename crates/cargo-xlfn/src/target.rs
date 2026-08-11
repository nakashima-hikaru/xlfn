use super::*;

pub(crate) fn built_library_path(
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

pub(crate) fn validate_bundle_output_names(
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

pub(crate) fn is_reserved_distribution_name(name: &str, artifact_name: &str) -> bool {
    name.eq_ignore_ascii_case(&format!("{artifact_name}.xll"))
        || name.eq_ignore_ascii_case("build-manifest.json")
}

#[cfg(target_os = "windows")]
pub(crate) fn retryable_windows_path_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
    ) || matches!(error.raw_os_error(), Some(5) | Some(32) | Some(33))
}

#[cfg(target_os = "windows")]
pub(crate) fn retry_windows_path_operation(
    mut operation: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
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
pub(crate) fn move_file_ex_with_retry(
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

pub(crate) fn rename_path(from: &Path, to: &Path) -> io::Result<()> {
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
