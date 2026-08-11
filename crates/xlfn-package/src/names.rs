use super::*;

pub(crate) fn validate_relative(field: &str, value: &str) -> PackageResult {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\\')
        || value.contains(':')
        || path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        Err(format!("{field} has unsafe path {value:?}").into())
    } else {
        Ok(())
    }
}

/// Rejects symlinks and Windows reparse points in every existing component of
/// a path. Missing trailing components are allowed so callers can validate a
/// destination before creating it. This is a path-based check: it rejects
/// links present during validation, but does not provide descriptor-relative
/// protection against a concurrent adversary replacing a checked component.
pub fn validate_path_components(path: &Path) -> PackageResult {
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if is_reparse_point(&metadata) && !is_trusted_system_alias(ancestor) => {
                return Err(format!(
                    "path component must not be a symlink or reparse point: {}",
                    ancestor.display()
                )
                .into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Validates a directory destination and all of its existing ancestors.
pub fn validate_directory_path(path: &Path) -> PackageResult {
    validate_path_components(path)?;
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if !metadata.is_dir() && !is_trusted_system_alias(ancestor) => {
                return Err(
                    format!("path component must be a directory: {}", ancestor.display()).into(),
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn is_trusted_system_alias(path: &Path) -> bool {
    let expected = match path {
        path if path == Path::new("/etc") => Path::new("/private/etc"),
        path if path == Path::new("/tmp") => Path::new("/private/tmp"),
        path if path == Path::new("/var") => Path::new("/private/var"),
        _ => return false,
    };
    fs::canonicalize(path).is_ok_and(|resolved| resolved == expected)
}

#[cfg(not(target_os = "macos"))]
pub(crate) const fn is_trusted_system_alias(_path: &Path) -> bool {
    false
}

pub(crate) fn windows_name_key(field: &str, name: &str) -> PackageResult<String> {
    validate_windows_basename_for(field, name)?;
    Ok(name.to_ascii_lowercase())
}

pub(crate) fn windows_dll_name_key(field: &str, name: &str) -> PackageResult<String> {
    let key = windows_name_key(field, name)?;
    if !name.to_ascii_lowercase().ends_with(".dll") {
        return Err(format!("{field} must be a DLL basename, got {name:?}").into());
    }
    Ok(key)
}

/// Validates a single output component against the portable ASCII subset of
/// Windows filename rules used by XLL packages.
pub fn validate_windows_basename(name: &str) -> PackageResult {
    validate_windows_basename_for("Windows basename", name)
}

pub(crate) fn validate_windows_basename_for(field: &str, name: &str) -> PackageResult {
    xlfn_common::validate_windows_basename(name).map_err(|error| {
        format!("{field} is not a valid portable Windows basename {name:?}: {error}").into()
    })
}

pub(crate) fn normalize_external_imports(imports: &[String]) -> PackageResult<BTreeSet<String>> {
    imports
        .iter()
        .map(|name| {
            validate_relative("external import", name)?;
            let path = Path::new(name);
            if path.file_name().and_then(|value| value.to_str()) != Some(name) {
                return Err(format!("external import must be a DLL basename, got {name:?}").into());
            }
            windows_dll_name_key("external import", name)
        })
        .collect()
}
