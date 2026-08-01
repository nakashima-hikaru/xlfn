//! Shared validation and staging for public and repository XLL packaging.

#![deny(unsafe_code)]

use fs_err as fs;
use object::FileKind;
use object::endian::LittleEndian as LE;
use object::pe::IMAGE_SCN_MEM_EXECUTE;
use object::read::pe::{
    ExportTarget, ImageNtHeaders, Import as PeImport, PeFile, PeFile32, PeFile64,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

pub type PackageResult<T = ()> = Result<T, PackageError>;
pub const SYSTEM_IMPORT_POLICY_VERSION: &str = "windows-system-v1";
const IMAGE_FILE_EXECUTABLE_IMAGE: u16 = 0x0002;
const IMAGE_FILE_SYSTEM: u16 = 0x1000;
const IMAGE_FILE_DLL: u16 = 0x2000;
pub const REQUIRED_XLL_EXPORTS: &[&str] = &[
    "xlAutoOpen",
    "xlAutoClose",
    "xlAutoFree12",
    "xlAddInManagerInfo12",
    "DllGetClassObject",
    "DllCanUnloadNow",
];

const CRT_MARKER_MAGIC: &[u8; 8] = b"XLFNCRT\0";
const CRT_MARKER_SCHEMA: u8 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectiveCrtPolicy {
    Dynamic,
    Static,
}

impl EffectiveCrtPolicy {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dynamic => "dynamic",
            Self::Static => "static",
        }
    }
}

fn parse_crt_marker(data: &[u8]) -> PackageResult<EffectiveCrtPolicy> {
    if data.len() < 16 || &data[..CRT_MARKER_MAGIC.len()] != CRT_MARKER_MAGIC {
        return Err("malformed .xlfncrt marker: marker must start at section offset 0".into());
    }
    if data
        .windows(CRT_MARKER_MAGIC.len())
        .filter(|candidate| *candidate == CRT_MARKER_MAGIC)
        .count()
        != 1
    {
        return Err("malformed .xlfncrt marker: multiple magic values".into());
    }
    if data[16..].iter().any(|byte| *byte != 0) {
        return Err("malformed .xlfncrt marker: non-zero section padding".into());
    }
    let marker = &data[..16];
    if marker[8] != CRT_MARKER_SCHEMA {
        return Err(format!("unsupported .xlfncrt marker schema {}", marker[8]).into());
    }
    if marker[10..].iter().any(|byte| *byte != 0) {
        return Err("malformed .xlfncrt marker: reserved bytes are not zero".into());
    }
    match marker[9] {
        0 => Ok(EffectiveCrtPolicy::Dynamic),
        1 => Ok(EffectiveCrtPolicy::Static),
        value => Err(format!("invalid .xlfncrt policy value {value}").into()),
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ImportTarget {
    Name(String),
    Ordinal(u16),
}

impl ImportTarget {
    fn display(&self) -> String {
        match self {
            Self::Name(name) => name.clone(),
            Self::Ordinal(ordinal) => format!("#{ordinal}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Pe(#[from] object::Error),
    #[error(transparent)]
    Utf8(#[from] std::str::Utf8Error),
    #[error("{target}: bundle source is busy or open for modification: {path}: {source}")]
    BundleSourceBusy {
        target: String,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{target}: bundle source changed while snapshotting: {path}")]
    UnstableBundleSource { target: String, path: PathBuf },
    #[error("{0}")]
    Message(String),
}

impl From<String> for PackageError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for PackageError {
    fn from(message: &str) -> Self {
        Self::Message(message.to_owned())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleMetadata {
    #[serde(default)]
    pub x86: Vec<String>,
    #[serde(default)]
    pub x64: Vec<String>,
    #[serde(default, rename = "external-imports")]
    pub external_imports: Vec<String>,
    #[serde(default = "default_strict_paths", rename = "strict-paths")]
    pub strict_paths: bool,
}

fn default_strict_paths() -> bool {
    true
}

impl Default for BundleMetadata {
    fn default() -> Self {
        Self {
            x86: Vec::new(),
            x64: Vec::new(),
            external_imports: Vec::new(),
            strict_paths: default_strict_paths(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Architecture {
    X86,
    X64,
}

impl Architecture {
    pub fn parse(target: &str) -> PackageResult<Self> {
        match target {
            "i686-pc-windows-msvc" => Ok(Self::X86),
            "x86_64-pc-windows-msvc" => Ok(Self::X64),
            _ => Err(format!("unsupported Windows target {target:?}").into()),
        }
    }
    pub const fn machine(self) -> u16 {
        match self {
            Self::X86 => 0x014c,
            Self::X64 => 0x8664,
        }
    }
}

#[derive(Clone)]
struct BundleFile {
    source: PathBuf,
    name: String,
    configured_path: String,
    snapshot: Option<Arc<[u8]>>,
    permissions: std::fs::Permissions,
}

impl std::fmt::Debug for BundleFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BundleFile")
            .field("source", &self.source)
            .field("name", &self.name)
            .field("configured_path", &self.configured_path)
            .field(
                "snapshot_len",
                &self.snapshot.as_deref().map(|snapshot| snapshot.len()),
            )
            .field("permissions", &self.permissions)
            .finish()
    }
}

#[derive(Debug)]
pub struct ResolvedBundle {
    files: Vec<BundleFile>,
    external_imports: BTreeSet<String>,
}

impl ResolvedBundle {
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            files: Vec::new(),
            external_imports: BTreeSet::new(),
        }
    }

    pub fn resolved_files(&self) -> impl Iterator<Item = (&str, &Path)> {
        self.files
            .iter()
            .map(|file| (file.configured_path.as_str(), file.source.as_path()))
    }

    pub fn external_imports(&self) -> impl Iterator<Item = &str> {
        self.external_imports.iter().map(String::as_str)
    }
}

#[derive(Clone, Debug)]
pub struct StagedBundle {
    files: Vec<BundleFile>,
    external_imports: BTreeSet<String>,
}

impl StagedBundle {
    pub fn try_add_external_imports(
        &mut self,
        imports: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> PackageResult {
        let normalized = imports
            .into_iter()
            .map(|name| windows_dll_name_key("external import", name.as_ref()))
            .collect::<PackageResult<Vec<_>>>()?;
        self.external_imports.extend(normalized);
        Ok(())
    }
}

pub struct VerifiedPackage {
    files: Vec<PathBuf>,
}

impl VerifiedPackage {
    #[must_use]
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }
}

pub fn resolve_bundle_files(
    manifest_directory: &Path,
    target: &str,
    configured: &[String],
) -> PackageResult<ResolvedBundle> {
    resolve_bundle_files_with_policy(manifest_directory, target, configured, &[], true)
}

pub fn resolve_bundle_files_with_metadata(
    manifest_directory: &Path,
    target: &str,
    metadata: &BundleMetadata,
) -> PackageResult<ResolvedBundle> {
    let configured = match Architecture::parse(target)? {
        Architecture::X86 => &metadata.x86,
        Architecture::X64 => &metadata.x64,
    };
    resolve_bundle_files_with_policy(
        manifest_directory,
        target,
        configured,
        &metadata.external_imports,
        metadata.strict_paths,
    )
}

pub fn resolve_bundle_files_with_policy(
    manifest_directory: &Path,
    target: &str,
    configured: &[String],
    external_imports: &[String],
    strict_paths: bool,
) -> PackageResult<ResolvedBundle> {
    resolve_bundle_files_with_policy_impl(
        manifest_directory,
        target,
        configured,
        external_imports,
        strict_paths,
        &NoopSnapshotObserver,
    )
}

fn resolve_bundle_files_with_policy_impl(
    manifest_directory: &Path,
    target: &str,
    configured: &[String],
    external_imports: &[String],
    strict_paths: bool,
    observer: &dyn SnapshotObserver,
) -> PackageResult<ResolvedBundle> {
    Architecture::parse(target)?;
    let canonical_root = fs::canonicalize(manifest_directory).map_err(|error| {
        PackageError::Message(format!(
            "failed to resolve manifest directory {}: {error}",
            manifest_directory.display()
        ))
    })?;
    let external_imports = normalize_external_imports(external_imports)?;
    let mut names = BTreeSet::new();
    let mut files = Vec::with_capacity(configured.len());
    for configured_path in configured {
        validate_relative("bundle file", configured_path)?;
        if strict_paths {
            reject_reparse_points(&canonical_root, configured_path)?;
        }
        let unresolved_source = canonical_root.join(configured_path);
        let mut opened_source = open_bundle_source_for_snapshot(&unresolved_source)
            .map_err(|error| map_snapshot_open_error(target, &unresolved_source, error))?;
        observer.after_open(&unresolved_source);
        let source = fs::canonicalize(&unresolved_source).map_err(|error| {
            PackageError::Message(format!(
                "{target}: failed to resolve {}: {error}",
                unresolved_source.display()
            ))
        })?;
        if !path_is_within(&canonical_root, &source) {
            return Err(format!(
                "{target}: bundle file escapes manifest directory: {} resolves to {}",
                unresolved_source.display(),
                source.display()
            )
            .into());
        }
        let opened_metadata = opened_source.metadata()?;
        if !opened_metadata.is_file() {
            return Err(format!("{target}: missing {}", source.display()).into());
        }
        let canonical_source = open_bundle_source_for_snapshot(&source)
            .map_err(|error| map_snapshot_open_error(target, &source, error))?;
        if !same_file_identity(&opened_source, &canonical_source)? {
            return Err(unstable_bundle_source(target, &unresolved_source));
        }
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("bundle file has no UTF-8 basename: {}", source.display()))?
            .to_owned();
        let name_key = windows_name_key("bundle file", &name)?;
        if is_system(&name_key) {
            return Err(format!("bundle must not shadow Windows system DLL {name:?}").into());
        }
        if !names.insert(name_key) {
            return Err(format!("duplicate bundle basename {name:?}").into());
        }
        let snapshot = read_stable_snapshot(target, &source, &mut opened_source, observer)?;
        #[cfg(not(target_os = "windows"))]
        verify_snapshot_against_second_read(target, &source, &mut opened_source, &snapshot)?;
        files.push(BundleFile {
            source,
            name,
            configured_path: configured_path.clone(),
            snapshot: Some(snapshot),
            permissions: opened_metadata.permissions(),
        });
    }
    Ok(ResolvedBundle {
        files,
        external_imports,
    })
}

pub fn verify_bundle_files(bundle: &ResolvedBundle, target: &str) -> PackageResult {
    validate_imports(
        &bundle.files,
        Architecture::parse(target)?,
        &bundle.external_imports,
    )
}

pub fn stage_bundle(bundle: &ResolvedBundle, destination: &Path) -> PackageResult<StagedBundle> {
    if destination.exists() || fs::symlink_metadata(destination).is_ok() {
        return Err(format!(
            "staging destination already exists: {}",
            destination.display()
        )
        .into());
    }
    let parent = destination.parent().ok_or_else(|| {
        PackageError::Message("staging destination has no parent directory".into())
    })?;
    fs::create_dir_all(parent)?;

    fs::create_dir(destination)?;
    let canonical_destination = fs::canonicalize(destination)?;

    let mut files = Vec::with_capacity(bundle.files.len());
    for file in &bundle.files {
        reject_reparse_points(&canonical_destination, &file.name)?;
        let output = destination.join(&file.name);
        if fs::symlink_metadata(&output).is_ok() {
            return Err(format!(
                "destination file already exists or is symlink: {}",
                output.display()
            )
            .into());
        }

        let mut temp_file = tempfile::Builder::new()
            .prefix(".stage-file-")
            .tempfile_in(destination)?;

        let temp_path = temp_file.path();
        let canonical_temp = fs::canonicalize(temp_path)?;
        if !path_is_within(&canonical_destination, &canonical_temp) {
            return Err(format!(
                "staging file escapes destination directory: {} vs {}",
                canonical_temp.display(),
                canonical_destination.display()
            )
            .into());
        }

        let snapshot = file.snapshot.as_deref().ok_or_else(|| {
            PackageError::Message(format!(
                "resolved bundle file has no immutable snapshot: {}",
                file.source.display()
            ))
        })?;
        temp_file.as_file_mut().write_all(snapshot)?;
        temp_file.as_file_mut().flush()?;
        temp_file
            .as_file_mut()
            .set_permissions(file.permissions.clone())?;

        temp_file.persist(&output).map_err(|err| {
            PackageError::Message(format!(
                "failed to place staged file {}: {err}",
                output.display()
            ))
        })?;

        let canonical_output = fs::canonicalize(&output)?;
        if !path_is_within(&canonical_destination, &canonical_output) {
            return Err(format!(
                "staged output file escapes destination directory: {} vs {}",
                canonical_output.display(),
                canonical_destination.display()
            )
            .into());
        }

        files.push(BundleFile {
            source: output,
            name: file.name.clone(),
            configured_path: file.configured_path.clone(),
            snapshot: None,
            permissions: file.permissions.clone(),
        });
    }
    Ok(StagedBundle {
        files,
        external_imports: bundle.external_imports.clone(),
    })
}

pub fn verify_staged_package(
    xll: &Path,
    target: &str,
    required_exports: &[String],
    bundle: StagedBundle,
) -> PackageResult<VerifiedPackage> {
    verify_xll(xll, target, required_exports)?;
    verify_dependency_closure(xll, target, &bundle)?;
    let mut files = Vec::with_capacity(bundle.files.len() + 1);
    files.push(xll.to_path_buf());
    files.extend(bundle.files.into_iter().map(|file| file.source));
    Ok(VerifiedPackage { files })
}

pub fn verify_xll(path: &Path, target: &str, required_exports: &[String]) -> PackageResult {
    let info = inspect_pe(path)?;
    verify_machine(&info, Architecture::parse(target)?, path)?;
    verify_image_characteristics(&info, path)?;
    verify_xll_exports(&info, path, required_exports)
}

fn verify_xll_exports(info: &PeInfo, path: &Path, required_exports: &[String]) -> PackageResult {
    if !info.has_export_manifest {
        return Err(format!(
            "{} is missing the .xllexp export manifest; ensure the crate has exactly one #[excel_addin]",
            path.display()
        )
        .into());
    }
    if info.crt_policy.is_none() {
        return Err(format!(
            "{} is missing the .xlfncrt effective CRT policy marker",
            path.display()
        )
        .into());
    }

    for export in REQUIRED_XLL_EXPORTS {
        if !info.exports.contains(*export) {
            return Err(format!("{} is missing export {export}", path.display()).into());
        }
        reject_forwarded_xll_export(info, path, export)?;
        if !info.executable_exports.contains(*export) {
            return Err(format!(
                "{} export {export} is not a direct executable target",
                path.display()
            )
            .into());
        }
        if !info.expected_exports.contains(*export) {
            return Err(format!(
                "{} has an incomplete .xllexp export manifest: missing {export}",
                path.display()
            )
            .into());
        }
    }
    for export in required_exports {
        if !info.exports.contains(export) {
            return Err(format!("{} is missing export {export}", path.display()).into());
        }
        reject_forwarded_xll_export(info, path, export)?;
        if !info.executable_exports.contains(export) {
            return Err(format!(
                "{} export {export} is not a direct executable target",
                path.display()
            )
            .into());
        }
    }

    let missing = info
        .expected_exports
        .difference(&info.exports)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "{} is missing expected export(s): {}",
            path.display(),
            missing.join(", ")
        )
        .into());
    }
    for export in &info.expected_exports {
        reject_forwarded_xll_export(info, path, export)?;
    }

    let non_executable = info
        .expected_exports
        .difference(&info.executable_exports)
        .cloned()
        .collect::<Vec<_>>();
    if !non_executable.is_empty() {
        return Err(format!(
            "{} has expected export(s) without a direct executable target: {}",
            path.display(),
            non_executable.join(", ")
        )
        .into());
    }

    // Closed-world validation: Reject any PE named export not present in expected_exports
    // (except loader entry points like _DllMainCRTStartup if present).
    let unexpected = info
        .exports
        .iter()
        .filter(|name| {
            !info.expected_exports.contains(*name) && name.as_str() != "_DllMainCRTStartup"
        })
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(format!(
            "{} has unexpected export(s): {}",
            path.display(),
            unexpected.join(", ")
        )
        .into());
    }

    let ordinal_only = info
        .nonzero_export_slots
        .difference(&info.named_export_slots)
        .collect::<Vec<_>>();
    if !ordinal_only.is_empty() {
        return Err(format!("{} has unmanifested ordinal-only export(s)", path.display()).into());
    }

    Ok(())
}

/// Validates the import closure rooted at the generated XLL and every bundled
/// DLL. Non-system imports must resolve to a case-insensitive basename in the
/// bundle, every imported name or ordinal must exist in that image, and every
/// PE image must match the requested architecture.
fn verify_dependency_closure(xll: &Path, target: &str, bundle: &StagedBundle) -> PackageResult {
    let architecture = Architecture::parse(target)?;
    let root_name = xll
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("XLL has no UTF-8 basename: {}", xll.display()))?;
    let mut images = BTreeMap::new();
    let root_key = windows_name_key("XLL basename", root_name)?;
    images.insert(
        root_key.clone(),
        (root_name.to_owned(), inspect_checked_pe(xll, architecture)?),
    );
    for file in bundle
        .files
        .iter()
        .filter(|file| file.name.to_ascii_lowercase().ends_with(".dll"))
    {
        let key = windows_dll_name_key("bundled DLL", &file.name)?;
        if key == root_key {
            return Err(format!(
                "bundled DLL basename `{}` collides with root XLL basename `{root_name}`",
                file.name
            )
            .into());
        }
        if images
            .insert(
                key,
                (
                    file.name.clone(),
                    inspect_checked_pe(&file.source, architecture)?,
                ),
            )
            .is_some()
        {
            return Err(format!("duplicate DLL basename in bundle: `{}`", file.name).into());
        }
    }
    validate_dependency_graph(&images, &bundle.external_imports)
}

/// Validates a standalone PE root and its colocated bundled DLL closure.
///
/// Every image must match `target`; non-system imports must resolve to one of
/// `bundled`, and every imported name or ordinal must exist in that image.
pub fn verify_pe_dependency_closure(
    root: &Path,
    target: &str,
    bundled: &[PathBuf],
) -> PackageResult {
    let architecture = Architecture::parse(target)?;
    let root_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("PE root has no UTF-8 basename: {}", root.display()))?;
    let root_key = windows_name_key("PE root basename", root_name)?;
    let mut images = BTreeMap::new();
    images.insert(
        root_key.clone(),
        (
            root_name.to_owned(),
            inspect_checked_pe(root, architecture)?,
        ),
    );

    for path in bundled {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("bundled DLL has no UTF-8 basename: {}", path.display()))?;
        let key = windows_dll_name_key("bundled DLL", name)?;
        if key == root_key {
            return Err(
                format!("bundled DLL basename `{name}` collides with root `{root_name}`").into(),
            );
        }
        if images
            .insert(
                key,
                (name.to_owned(), inspect_checked_pe(path, architecture)?),
            )
            .is_some()
        {
            return Err(format!("duplicate DLL basename in bundle: `{name}`").into());
        }
    }

    validate_dependency_graph(&images, &BTreeSet::new())
}

pub fn sha256(path: &Path) -> PackageResult<String> {
    let mut hash = Sha256::new();
    let mut file = fs::File::open(path)?;
    struct DigestWriter<'a>(&'a mut Sha256);
    impl io::Write for DigestWriter<'_> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.update(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    io::copy(&mut file, &mut DigestWriter(&mut hash))?;
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn validate_imports(
    files: &[BundleFile],
    architecture: Architecture,
    external_imports: &BTreeSet<String>,
) -> PackageResult {
    let mut images = BTreeMap::new();
    for file in files
        .iter()
        .filter(|file| file.name.to_ascii_lowercase().ends_with(".dll"))
    {
        let key = windows_dll_name_key("bundled DLL", &file.name)?;
        images.insert(
            key,
            (
                file.name.clone(),
                inspect_checked_bundle_file(file, architecture)?,
            ),
        );
    }
    validate_dependency_graph(&images, external_imports)
}

fn inspect_checked_pe(path: &Path, architecture: Architecture) -> PackageResult<PeInfo> {
    let info = inspect_pe(path)?;
    verify_machine(&info, architecture, path)?;
    verify_image_characteristics(&info, path)?;
    Ok(info)
}

fn inspect_checked_bundle_file(
    file: &BundleFile,
    architecture: Architecture,
) -> PackageResult<PeInfo> {
    let info = match file.snapshot.as_deref() {
        Some(snapshot) => parse_pe_bytes(snapshot)?,
        None => inspect_pe(&file.source)?,
    };
    verify_machine(&info, architecture, &file.source)?;
    verify_image_characteristics(&info, &file.source)?;
    Ok(info)
}

fn validate_dependency_graph(
    images: &BTreeMap<String, (String, PeInfo)>,
    external_imports: &BTreeSet<String>,
) -> PackageResult {
    for root in images.keys() {
        let mut path = vec![root.clone()];
        validate_dependency_node(
            root,
            images,
            external_imports,
            &mut path,
            &mut BTreeSet::new(),
        )?;
    }
    validate_forwarded_exports(images, external_imports)
}

fn validate_dependency_node(
    current: &str,
    images: &BTreeMap<String, (String, PeInfo)>,
    external_imports: &BTreeSet<String>,
    path: &mut Vec<String>,
    visited: &mut BTreeSet<String>,
) -> PackageResult {
    if !visited.insert(current.to_owned()) {
        return Ok(());
    }
    let (_, image) = images
        .get(current)
        .ok_or_else(|| format!("internal dependency graph error for {current}"))?;
    let imports = image
        .imports
        .iter()
        .map(|name| (name, image.import_targets.get(name)))
        .chain(
            image
                .delay_imports
                .iter()
                .map(|name| (name, image.delay_import_targets.get(name))),
        );
    for (imported_name, targets) in imports {
        let imported = windows_dll_name_key("PE import", imported_name)?;
        if is_system(&imported) || external_imports.contains(&imported) {
            continue;
        }
        let Some((imported_display, imported_image)) = images.get(&imported) else {
            let mut chain = path
                .iter()
                .filter_map(|name| images.get(name).map(|(display, _)| display.as_str()))
                .collect::<Vec<_>>();
            chain.push(imported.as_str());
            return Err(format!(
                "unresolved package import (policy {SYSTEM_IMPORT_POLICY_VERSION}): {}",
                chain.join(" -> ")
            )
            .into());
        };

        if let Some(targets) = targets {
            for target in targets {
                let exists = match target {
                    ImportTarget::Name(name) => imported_image.exports.contains(name),
                    ImportTarget::Ordinal(ordinal) => {
                        imported_image.exported_ordinals.contains(ordinal)
                    }
                };
                if !exists {
                    let mut chain = path
                        .iter()
                        .filter_map(|name| images.get(name).map(|(display, _)| display.clone()))
                        .collect::<Vec<_>>();
                    chain.push(format!("{imported_display}!{}", target.display()));
                    return Err(format!(
                        "unresolved package import target (policy {SYSTEM_IMPORT_POLICY_VERSION}): {}",
                        chain.join(" -> ")
                    )
                    .into());
                }
            }
        }

        path.push(imported.clone());
        validate_dependency_node(&imported, images, external_imports, path, visited)?;
        path.pop();
    }
    Ok(())
}

fn validate_forwarded_exports(
    images: &BTreeMap<String, (String, PeInfo)>,
    external_imports: &BTreeSet<String>,
) -> PackageResult {
    let mut resolved = BTreeSet::new();
    for (image_name, (_, image)) in images {
        for symbol in image.forwarded_exports.keys() {
            let mut stack = Vec::new();
            validate_forwarded_symbol(
                image_name,
                symbol,
                images,
                external_imports,
                &mut stack,
                &mut resolved,
            )?;
        }
    }
    Ok(())
}

fn validate_forwarded_symbol(
    image_name: &str,
    symbol: &ExportSymbol,
    images: &BTreeMap<String, (String, PeInfo)>,
    external_imports: &BTreeSet<String>,
    stack: &mut Vec<(String, ExportSymbol)>,
    resolved: &mut BTreeSet<(String, ExportSymbol)>,
) -> PackageResult {
    let node = (image_name.to_owned(), symbol.clone());
    if resolved.contains(&node) {
        return Ok(());
    }
    if let Some(position) = stack.iter().position(|entry| entry == &node) {
        let mut cycle = stack[position..]
            .iter()
            .map(|(image, symbol)| format!("{image}!{}", format_export_symbol(symbol)))
            .collect::<Vec<_>>();
        cycle.push(format!("{image_name}!{}", format_export_symbol(symbol)));
        return Err(format!("cyclic forwarded export: {}", cycle.join(" -> ")).into());
    }

    let (_, image) = images
        .get(image_name)
        .ok_or_else(|| format!("internal forwarded-export graph error for {image_name}"))?;
    let Some(forwarded) = image.forwarded_exports.get(symbol) else {
        if image.has_export_symbol(symbol) {
            resolved.insert(node);
            return Ok(());
        }
        return Err(format!(
            "{} is missing forwarded export target {}",
            images
                .get(image_name)
                .map_or(image_name, |(display, _)| display.as_str()),
            format_export_symbol(symbol)
        )
        .into());
    };

    stack.push(node.clone());
    let target_library = forwarded.library.to_ascii_lowercase();
    if !is_system(&target_library) && !external_imports.contains(&target_library) {
        let Some((target_display, target_image)) = images.get(&target_library) else {
            return Err(format!(
                "unresolved forwarded export: {}!{} -> {}!{}",
                images
                    .get(image_name)
                    .map_or(image_name, |(display, _)| display.as_str()),
                format_export_symbol(symbol),
                forwarded.library,
                format_export_symbol(&forwarded.symbol)
            )
            .into());
        };
        if !target_image.has_export_symbol(&forwarded.symbol) {
            return Err(format!(
                "forwarded export target is missing: {}!{} -> {target_display}!{}",
                images
                    .get(image_name)
                    .map_or(image_name, |(display, _)| display.as_str()),
                format_export_symbol(symbol),
                format_export_symbol(&forwarded.symbol)
            )
            .into());
        }
        validate_forwarded_symbol(
            &target_library,
            &forwarded.symbol,
            images,
            external_imports,
            stack,
            resolved,
        )?;
    }
    let _ = stack.pop();
    resolved.insert(node);
    Ok(())
}

fn format_export_symbol(symbol: &ExportSymbol) -> String {
    match symbol {
        ExportSymbol::Name(name) => name.clone(),
        ExportSymbol::Ordinal(ordinal) => format!("#{ordinal}"),
    }
}

fn validate_relative(field: &str, value: &str) -> PackageResult {
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

fn windows_name_key(field: &str, name: &str) -> PackageResult<String> {
    validate_windows_basename_for(field, name)?;
    Ok(name.to_ascii_lowercase())
}

fn windows_dll_name_key(field: &str, name: &str) -> PackageResult<String> {
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

fn validate_windows_basename_for(field: &str, name: &str) -> PackageResult {
    if name.is_empty()
        || name == "."
        || name == ".."
        || !name.is_ascii()
        || name.ends_with(' ')
        || name.ends_with('.')
        || name
            .chars()
            .any(|character| character <= '\u{1f}' || r#"<>:"/\|?*"#.contains(character))
    {
        return Err(format!(
            "{field} is outside the portable ASCII Windows basename subset: {name:?}"
        )
        .into());
    }

    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || stem.strip_prefix("COM").is_some_and(|suffix| {
        matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
    }) || stem.strip_prefix("LPT").is_some_and(|suffix| {
        matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
    });
    if reserved {
        return Err(format!("{field} uses a reserved Windows device name: {name:?}").into());
    }

    Ok(())
}

fn normalize_external_imports(imports: &[String]) -> PackageResult<BTreeSet<String>> {
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

fn path_is_within(root: &Path, candidate: &Path) -> bool {
    let mut candidate_components = candidate.components();
    root.components().all(|root_component| {
        candidate_components
            .next()
            .is_some_and(|candidate_component| component_eq(root_component, candidate_component))
    })
}

trait SnapshotObserver {
    fn after_open(&self, _path: &Path) {}

    fn after_first_chunk(&self, _path: &Path) {}
}

struct NoopSnapshotObserver;

impl SnapshotObserver for NoopSnapshotObserver {}

fn open_bundle_source_for_snapshot(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

        // Other readers remain allowed, but writers and delete/rename operations
        // are rejected while this handle is alive.
        options.share_mode(FILE_SHARE_READ);
    }

    options.open(path)
}

fn map_snapshot_open_error(target: &str, path: &Path, error: io::Error) -> PackageError {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;

        if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) {
            return PackageError::BundleSourceBusy {
                target: target.to_owned(),
                path: path.to_owned(),
                source: error,
            };
        }
    }

    PackageError::Message(format!(
        "{target}: failed to open {}: {error}",
        path.display()
    ))
}

fn unstable_bundle_source(target: &str, path: &Path) -> PackageError {
    PackageError::UnstableBundleSource {
        target: target.to_owned(),
        path: path.to_owned(),
    }
}

fn read_stable_snapshot(
    target: &str,
    path: &Path,
    file: &mut std::fs::File,
    observer: &dyn SnapshotObserver,
) -> PackageResult<Arc<[u8]>> {
    let before = file_snapshot_state(file)?;
    let expected_len = usize::try_from(before.len).map_err(|_| {
        PackageError::Message(format!(
            "{target}: bundle file is too large to snapshot: {}",
            path.display()
        ))
    })?;

    let mut snapshot = Vec::new();
    snapshot.try_reserve_exact(expected_len).map_err(|_| {
        PackageError::Message(format!(
            "{target}: cannot reserve {expected_len} bytes for bundle snapshot: {}",
            path.display()
        ))
    })?;

    file.seek(SeekFrom::Start(0))?;
    let mut limited = file.take(before.len.saturating_add(1));
    let mut buffer = [0_u8; 64 * 1024];
    let mut observed_first_chunk = false;

    loop {
        let count = limited.read(&mut buffer)?;
        if count == 0 {
            break;
        }

        if !observed_first_chunk {
            observed_first_chunk = true;
            observer.after_first_chunk(path);
        }

        let Some(new_len) = snapshot.len().checked_add(count) else {
            return Err(unstable_bundle_source(target, path));
        };
        if new_len > expected_len {
            return Err(unstable_bundle_source(target, path));
        }
        snapshot.extend_from_slice(&buffer[..count]);
    }

    if snapshot.len() as u64 != before.len {
        return Err(unstable_bundle_source(target, path));
    }

    let after = file_snapshot_state(file)?;
    if before != after {
        return Err(unstable_bundle_source(target, path));
    }

    Ok(Arc::from(snapshot))
}

#[cfg(not(target_os = "windows"))]
fn verify_snapshot_against_second_read(
    target: &str,
    path: &Path,
    file: &mut std::fs::File,
    expected: &[u8],
) -> PackageResult {
    let before = file_snapshot_state(file)?;
    file.seek(SeekFrom::Start(0))?;

    let expected_digest = Sha256::digest(expected);
    let mut hasher = Sha256::new();
    let mut limited = file.take(before.len.saturating_add(1));
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let count = limited.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    let observed_digest = hasher.finalize();
    let after = file_snapshot_state(file)?;
    if before != after || expected_digest != observed_digest {
        return Err(unstable_bundle_source(target, path));
    }

    Ok(())
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    volume_serial_number: u32,
    file_index: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileSnapshotState {
    #[cfg(any(unix, target_os = "windows"))]
    identity: FileIdentity,
    len: u64,
    #[cfg(unix)]
    mtime: i64,
    #[cfg(unix)]
    mtime_nsec: i64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
    #[cfg(target_os = "windows")]
    last_write_time: u64,
}

#[cfg(unix)]
fn file_snapshot_state(file: &std::fs::File) -> io::Result<FileSnapshotState> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata()?;
    Ok(FileSnapshotState {
        identity: FileIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        },
        len: metadata.len(),
        mtime: metadata.mtime(),
        mtime_nsec: metadata.mtime_nsec(),
        ctime: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
    })
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
fn file_snapshot_state(file: &std::fs::File) -> io::Result<FileSnapshotState> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    let handle = file.as_raw_handle() as HANDLE;
    // SAFETY: `handle` remains valid for the duration of the call because it is borrowed
    // from `file`, and `information` points to writable storage of the required type.
    let status = unsafe { GetFileInformationByHandle(handle, information.as_mut_ptr()) };
    if status == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful `GetFileInformationByHandle` initializes the complete
    // `BY_HANDLE_FILE_INFORMATION` output structure.
    let information = unsafe { information.assume_init() };

    let file_index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    let len = (u64::from(information.nFileSizeHigh) << 32) | u64::from(information.nFileSizeLow);
    let last_write_time = (u64::from(information.ftLastWriteTime.dwHighDateTime) << 32)
        | u64::from(information.ftLastWriteTime.dwLowDateTime);

    Ok(FileSnapshotState {
        identity: FileIdentity {
            volume_serial_number: information.dwVolumeSerialNumber,
            file_index,
        },
        len,
        last_write_time,
    })
}

#[cfg(not(any(unix, target_os = "windows")))]
fn file_snapshot_state(file: &std::fs::File) -> io::Result<FileSnapshotState> {
    Ok(FileSnapshotState {
        len: file.metadata()?.len(),
    })
}

#[cfg(unix)]
fn same_file_identity(left: &std::fs::File, right: &std::fs::File) -> io::Result<bool> {
    Ok(file_snapshot_state(left)?.identity == file_snapshot_state(right)?.identity)
}

#[cfg(target_os = "windows")]
fn same_file_identity(left: &std::fs::File, right: &std::fs::File) -> io::Result<bool> {
    Ok(file_snapshot_state(left)?.identity == file_snapshot_state(right)?.identity)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn same_file_identity(left: &std::fs::File, right: &std::fs::File) -> io::Result<bool> {
    // The supported release hosts are Unix and Windows. Keep other targets
    // conservative rather than claiming identity from path metadata alone.
    let _ = (left, right);
    Ok(false)
}

fn component_eq(left: Component<'_>, right: Component<'_>) -> bool {
    #[cfg(target_os = "windows")]
    {
        left.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
    }
    #[cfg(not(target_os = "windows"))]
    {
        left == right
    }
}

fn reject_reparse_points(root: &Path, configured_path: &str) -> PackageResult {
    let mut current = root.to_path_buf();
    for component in Path::new(configured_path).components() {
        let Component::Normal(component) = component else {
            return Err(format!("bundle file has unsafe path {configured_path:?}").into());
        };
        current.push(component);
        if let Ok(metadata) = fs::symlink_metadata(&current)
            && is_reparse_point(&metadata)
        {
            return Err(format!(
                "strict bundle path rejects symlink or reparse point: {}",
                current.display()
            )
            .into());
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(target_os = "windows"))]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}
fn verify_machine(info: &PeInfo, architecture: Architecture, path: &Path) -> PackageResult {
    if info.machine == architecture.machine() {
        Ok(())
    } else {
        Err(format!("{} has wrong PE machine", path.display()).into())
    }
}
fn verify_image_characteristics(info: &PeInfo, path: &Path) -> PackageResult {
    if info.characteristics & IMAGE_FILE_EXECUTABLE_IMAGE == 0 {
        return Err(format!("{} is not an executable PE image", path.display()).into());
    }
    if info.characteristics & IMAGE_FILE_DLL == 0 {
        return Err(format!("{} is not marked as a PE DLL", path.display()).into());
    }
    if info.characteristics & IMAGE_FILE_SYSTEM != 0 {
        return Err(format!("{} is marked as a system image", path.display()).into());
    }
    Ok(())
}

fn reject_forwarded_xll_export(info: &PeInfo, path: &Path, export: &str) -> PackageResult {
    let symbol = ExportSymbol::Name(export.to_owned());
    if let Some(forwarded) = info.forwarded_exports.get(&symbol) {
        return Err(format!(
            "{} forwards required XLL export {export} to {}!{}; XLL entry points must be implemented directly",
            path.display(),
            forwarded.library,
            format_export_symbol(&forwarded.symbol)
        )
        .into());
    }
    Ok(())
}
fn is_system(name: &str) -> bool {
    const SYSTEM_DLLS: &[&str] = &[
        "advapi32.dll",
        "api-ms-win-core-synch-l1-1-0.dll",
        "api-ms-win-core-synch-l1-2-0.dll",
        "bcrypt.dll",
        "bcryptprimitives.dll",
        "cabinet.dll",
        "cfgmgr32.dll",
        "comctl32.dll",
        "comdlg32.dll",
        "combase.dll",
        "crypt32.dll",
        "d2d1.dll",
        "d3d11.dll",
        "dwrite.dll",
        "dwmapi.dll",
        "dxgi.dll",
        "gdi32.dll",
        "imm32.dll",
        "iphlpapi.dll",
        "kernel32.dll",
        "mpr.dll",
        "msvcrt.dll",
        "netapi32.dll",
        "ntdll.dll",
        "ole32.dll",
        "oleaut32.dll",
        "powrprof.dll",
        "psapi.dll",
        "rpcrt4.dll",
        "secur32.dll",
        "setupapi.dll",
        "shell32.dll",
        "shlwapi.dll",
        "ucrtbase.dll",
        "user32.dll",
        "userenv.dll",
        "uxtheme.dll",
        "version.dll",
        "winhttp.dll",
        "wininet.dll",
        "winmm.dll",
        "wintrust.dll",
        "ws2_32.dll",
    ];

    SYSTEM_DLLS.contains(&name)
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ExportSymbol {
    Name(String),
    Ordinal(u16),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForwardedExport {
    pub library: String,
    pub symbol: ExportSymbol,
}

#[derive(Clone, Debug)]
pub struct PeInfo {
    pub machine: u16,
    pub characteristics: u16,
    pub exports: BTreeSet<String>,
    pub forwarded_exports: BTreeMap<ExportSymbol, ForwardedExport>,
    pub executable_exports: BTreeSet<String>,
    pub has_export_manifest: bool,
    pub expected_exports: BTreeSet<String>,
    pub crt_policy: Option<EffectiveCrtPolicy>,
    pub imports: BTreeSet<String>,
    pub import_targets: BTreeMap<String, BTreeSet<ImportTarget>>,
    pub delay_imports: BTreeSet<String>,
    pub delay_import_targets: BTreeMap<String, BTreeSet<ImportTarget>>,
    /// Ordinals represented by non-zero export address table entries.
    pub exported_ordinals: BTreeSet<u16>,
    /// Non-zero export address table indices, used for closed-world validation.
    pub nonzero_export_slots: BTreeSet<u32>,
    /// Export address table indices referenced by at least one export name.
    pub named_export_slots: BTreeSet<u32>,
}

impl Default for PeInfo {
    fn default() -> Self {
        Self {
            machine: 0x8664,
            characteristics: 0,
            exports: BTreeSet::new(),
            forwarded_exports: BTreeMap::new(),
            executable_exports: BTreeSet::new(),
            has_export_manifest: true,
            expected_exports: BTreeSet::new(),
            crt_policy: None,
            imports: BTreeSet::new(),
            import_targets: BTreeMap::new(),
            delay_imports: BTreeSet::new(),
            delay_import_targets: BTreeMap::new(),
            exported_ordinals: BTreeSet::new(),
            nonzero_export_slots: BTreeSet::new(),
            named_export_slots: BTreeSet::new(),
        }
    }
}

impl PeInfo {
    fn has_export_symbol(&self, symbol: &ExportSymbol) -> bool {
        match symbol {
            ExportSymbol::Name(name) => self.exports.contains(name),
            ExportSymbol::Ordinal(ordinal) => self.exported_ordinals.contains(ordinal),
        }
    }
}
pub fn inspect_pe(path: &Path) -> PackageResult<PeInfo> {
    parse_pe_bytes(&fs::read(path)?)
}

pub fn parse_pe_bytes(b: &[u8]) -> PackageResult<PeInfo> {
    match FileKind::parse(b)? {
        FileKind::Pe32 => parse_pe_file(PeFile32::parse(b)?),
        FileKind::Pe64 => parse_pe_file(PeFile64::parse(b)?),
        _ => Err("file is not a PE32 or PE32+ image".into()),
    }
}

fn normalize_forwarder_library(library: &str) -> PackageResult<String> {
    validate_relative("forwarded export library", library)?;
    let path = Path::new(library);
    if path.file_name().and_then(|value| value.to_str()) != Some(library) {
        return Err(format!("forwarded export library must be a basename, got {library:?}").into());
    }
    let mut normalized = library.to_ascii_lowercase();
    if !normalized.contains('.') {
        normalized.push_str(".dll");
    } else if !normalized.ends_with(".dll") && !normalized.ends_with(".xll") {
        return Err(format!("forwarded export library has invalid name {library:?}").into());
    }
    Ok(normalized)
}

fn parse_pe_file<Pe>(pe: PeFile<'_, Pe>) -> PackageResult<PeInfo>
where
    Pe: ImageNtHeaders,
{
    let file_header = pe.nt_headers().file_header();
    let machine = file_header.machine.get(LE);
    let characteristics = file_header.characteristics.get(LE);
    let mut exports = BTreeSet::new();
    let mut executable_exports = BTreeSet::new();
    let mut forwarded_exports = BTreeMap::new();
    let mut exported_ordinals = BTreeSet::new();
    let mut nonzero_export_slots = BTreeSet::new();
    let mut named_export_slots = BTreeSet::new();
    if let Some(table) = pe.export_table()? {
        if table.ordinal_base() > u32::from(u16::MAX) {
            return Err(format!(
                "PE export ordinal base {} exceeds u16",
                table.ordinal_base()
            )
            .into());
        }
        let mut export_targets = Vec::with_capacity(table.addresses().len());
        for (index, address) in table.addresses().iter().enumerate() {
            let address = address.get(LE);
            if address == 0 {
                export_targets.push(None);
                continue;
            }

            let ordinal_index = u32::try_from(index)
                .map_err(|_| PackageError::Message("PE export table is too large".into()))?;
            let ordinal = table
                .ordinal_base()
                .checked_add(ordinal_index)
                .ok_or_else(|| PackageError::Message("PE export ordinal overflows".into()))?;
            let exported_ordinal = u16::try_from(ordinal)
                .map_err(|_| format!("PE export ordinal {ordinal} exceeds u16"))?;
            nonzero_export_slots.insert(ordinal_index);
            exported_ordinals.insert(exported_ordinal);
            let executable = match table.target_from_address(address)? {
                ExportTarget::Address(address) => {
                    let section = pe
                        .section_table()
                        .iter()
                        .find(|section| {
                            let start = section.virtual_address.get(LE);
                            let size = section
                                .virtual_size
                                .get(LE)
                                .max(section.size_of_raw_data.get(LE));
                            address >= start && address < start.saturating_add(size)
                        })
                        .ok_or_else(|| {
                            PackageError::Message(format!(
                                "PE export ordinal {ordinal} points outside every mapped section"
                            ))
                        })?;
                    section.characteristics.get(LE) & IMAGE_SCN_MEM_EXECUTE != 0
                }
                ExportTarget::ForwardByOrdinal(library, target_ordinal) => {
                    let forwarded = ForwardedExport {
                        library: normalize_forwarder_library(std::str::from_utf8(library)?)?,
                        symbol: ExportSymbol::Ordinal(u16::try_from(target_ordinal).map_err(
                            |_| format!("forwarded export ordinal {target_ordinal} exceeds u16"),
                        )?),
                    };
                    forwarded_exports.insert(ExportSymbol::Ordinal(exported_ordinal), forwarded);
                    false
                }
                ExportTarget::ForwardByName(library, target_name) => {
                    let forwarded = ForwardedExport {
                        library: normalize_forwarder_library(std::str::from_utf8(library)?)?,
                        symbol: ExportSymbol::Name(std::str::from_utf8(target_name)?.to_owned()),
                    };
                    forwarded_exports.insert(ExportSymbol::Ordinal(exported_ordinal), forwarded);
                    false
                }
            };

            export_targets.push(Some(executable));
        }

        for (name_pointer, ordinal_index) in table.name_iter() {
            named_export_slots.insert(u32::from(ordinal_index));
            let target = export_targets
                .get(usize::from(ordinal_index))
                .copied()
                .ok_or_else(|| PackageError::Message("invalid PE export ordinal index".into()))?;
            let Some(executable) = target else {
                // A name attached to a zero EAT entry is not a resolvable
                // export and must not satisfy lifecycle or import validation.
                continue;
            };
            let name = std::str::from_utf8(table.name_from_pointer(name_pointer)?)?.to_owned();
            if executable {
                executable_exports.insert(name.clone());
            }
            exports.insert(name);
        }
    }

    let mut has_export_manifest = false;
    let mut expected_exports = BTreeSet::new();
    let mut crt_policy = None;
    for section in pe.section_table().iter() {
        let name_str = std::str::from_utf8(&section.name)
            .unwrap_or("")
            .trim_matches('\0');
        let is_export_manifest = name_str == ".xllexp" || name_str.ends_with(".xllexp");
        if is_export_manifest {
            has_export_manifest = true;
        }
        if is_export_manifest && let Ok(data) = section.pe_data(pe.data()) {
            for part in data.split(|&b| b == 0) {
                if !part.is_empty()
                    && let Ok(export_name) = std::str::from_utf8(part)
                {
                    let trimmed = export_name.trim();
                    if !trimmed.is_empty() {
                        expected_exports.insert(trimmed.to_owned());
                    }
                }
            }
        }
        if name_str == ".xlfncrt" || name_str.ends_with(".xlfncrt") {
            let data = section.pe_data(pe.data()).map_err(|error| {
                PackageError::Message(format!("failed to read .xlfncrt section: {error}"))
            })?;
            let observed = parse_crt_marker(data)?;
            if crt_policy.replace(observed).is_some() {
                return Err("PE image contains multiple .xlfncrt markers".into());
            }
        }
    }

    let mut imports = BTreeSet::new();
    let mut import_targets: BTreeMap<String, BTreeSet<ImportTarget>> = BTreeMap::new();
    if let Some(table) = pe.import_table()? {
        let mut descriptors = table.descriptors()?;
        while let Some(descriptor) = descriptors.next()? {
            let name = std::str::from_utf8(table.name(descriptor.name.get(LE))?)?.to_owned();
            let lookup_rva = {
                let original = descriptor.original_first_thunk.get(LE);
                if original == 0 {
                    descriptor.first_thunk.get(LE)
                } else {
                    original
                }
            };
            let mut targets = BTreeSet::new();
            if lookup_rva != 0 {
                let mut thunks = table.thunks(lookup_rva)?;
                while let Some(thunk) = thunks.next::<Pe>()? {
                    match table.import::<Pe>(thunk)? {
                        PeImport::Ordinal(ordinal) => {
                            targets.insert(ImportTarget::Ordinal(ordinal));
                        }
                        PeImport::Name(_, symbol) => {
                            targets.insert(ImportTarget::Name(
                                std::str::from_utf8(symbol)?.to_owned(),
                            ));
                        }
                    }
                }
            }
            imports.insert(name.clone());
            import_targets.entry(name).or_default().extend(targets);
        }
    }

    let mut delay_imports = BTreeSet::new();
    let mut delay_import_targets: BTreeMap<String, BTreeSet<ImportTarget>> = BTreeMap::new();
    if let Some(table) = pe
        .data_directories()
        .delay_load_import_table(pe.data(), &pe.section_table())?
    {
        let mut descriptors = table.descriptors()?;
        while let Some(descriptor) = descriptors.next()? {
            if descriptor.attributes.get(LE) & 1 == 0 {
                return Err("unsupported VA-based delay load import descriptor".into());
            }
            let name =
                std::str::from_utf8(table.name(descriptor.dll_name_rva.get(LE))?)?.to_owned();
            let lookup_rva = descriptor.import_name_table_rva.get(LE);
            if lookup_rva == 0 {
                return Err(format!("delay import {name:?} has no import name table").into());
            }
            let mut targets = BTreeSet::new();
            let mut thunks = table.thunks(lookup_rva)?;
            while let Some(thunk) = thunks.next::<Pe>()? {
                match table.import::<Pe>(thunk)? {
                    PeImport::Ordinal(ordinal) => {
                        targets.insert(ImportTarget::Ordinal(ordinal));
                    }
                    PeImport::Name(_, symbol) => {
                        targets.insert(ImportTarget::Name(std::str::from_utf8(symbol)?.to_owned()));
                    }
                }
            }
            delay_imports.insert(name.clone());
            delay_import_targets
                .entry(name)
                .or_default()
                .extend(targets);
        }
    }

    Ok(PeInfo {
        machine,
        characteristics,
        exports,
        forwarded_exports,
        executable_exports,
        has_export_manifest,
        expected_exports,
        crt_policy,
        imports,
        import_targets,
        delay_imports,
        delay_import_targets,
        exported_ordinals,
        nonzero_export_slots,
        named_export_slots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn xll_info() -> PeInfo {
        let framework = REQUIRED_XLL_EXPORTS
            .iter()
            .copied()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        PeInfo {
            machine: Architecture::X64.machine(),
            characteristics: IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL,
            exports: framework.clone(),
            forwarded_exports: BTreeMap::new(),
            executable_exports: framework.clone(),
            has_export_manifest: true,
            expected_exports: framework,
            crt_policy: Some(EffectiveCrtPolicy::Dynamic),
            imports: BTreeSet::new(),
            import_targets: BTreeMap::new(),
            delay_imports: BTreeSet::new(),
            delay_import_targets: BTreeMap::new(),
            exported_ordinals: BTreeSet::new(),
            nonzero_export_slots: BTreeSet::new(),
            named_export_slots: BTreeSet::new(),
        }
    }

    #[test]
    fn crt_marker_records_the_effective_compiler_policy() {
        let mut dynamic = [0_u8; 16];
        dynamic[..8].copy_from_slice(CRT_MARKER_MAGIC);
        dynamic[8] = CRT_MARKER_SCHEMA;
        assert_eq!(
            parse_crt_marker(&dynamic).unwrap(),
            EffectiveCrtPolicy::Dynamic
        );

        dynamic[9] = 1;
        assert_eq!(
            parse_crt_marker(&dynamic).unwrap(),
            EffectiveCrtPolicy::Static
        );
        dynamic[8] = 2;
        assert!(parse_crt_marker(&dynamic).is_err());
    }

    #[test]
    fn crt_marker_requires_a_canonical_section_layout() {
        let mut marker = [0_u8; 16];
        marker[..8].copy_from_slice(CRT_MARKER_MAGIC);
        marker[8] = CRT_MARKER_SCHEMA;

        let mut junk_prefix = vec![0x5a];
        junk_prefix.extend_from_slice(&marker);
        assert!(parse_crt_marker(&junk_prefix).is_err());

        let mut duplicate = marker.to_vec();
        duplicate.extend_from_slice(&marker);
        assert!(parse_crt_marker(&duplicate).is_err());

        let mut reserved = marker;
        reserved[10] = 1;
        assert!(parse_crt_marker(&reserved).is_err());

        let mut padding = marker.to_vec();
        padding.extend_from_slice(&[0; 8]);
        assert_eq!(
            parse_crt_marker(&padding).unwrap(),
            EffectiveCrtPolicy::Dynamic
        );
    }

    #[test]
    fn xll_verification_requires_the_framework_manifest_and_lifecycle_exports() {
        let path = Path::new("addin.xll");
        let mut missing_manifest = xll_info();
        missing_manifest.has_export_manifest = false;
        assert!(
            verify_xll_exports(&missing_manifest, path, &[])
                .unwrap_err()
                .to_string()
                .contains(".xllexp")
        );

        let mut missing_crt_marker = xll_info();
        missing_crt_marker.crt_policy = None;
        assert!(
            verify_xll_exports(&missing_crt_marker, path, &[])
                .unwrap_err()
                .to_string()
                .contains(".xlfncrt")
        );

        let mut missing_lifecycle = xll_info();
        missing_lifecycle.exports.remove("xlAutoOpen");
        assert!(
            verify_xll_exports(&missing_lifecycle, path, &[])
                .unwrap_err()
                .to_string()
                .contains("xlAutoOpen")
        );

        assert!(verify_xll_exports(&xll_info(), path, &[]).is_ok());
    }

    #[test]
    fn xll_verification_reconciles_manifest_and_actual_udf_exports() {
        let path = Path::new("addin.xll");
        let mut info = xll_info();
        info.expected_exports.insert("xll_compute".to_owned());
        assert!(
            verify_xll_exports(&info, path, &[])
                .unwrap_err()
                .to_string()
                .contains("xll_compute")
        );
        info.exports.insert("xll_compute".to_owned());
        info.executable_exports.insert("xll_compute".to_owned());
        assert!(verify_xll_exports(&info, path, &["xll_compute".to_owned()]).is_ok());
    }

    #[test]
    fn xll_verification_rejects_forwarded_entry_points() {
        let path = Path::new("addin.xll");
        let mut info = xll_info();
        let forwarded = ForwardedExport {
            library: "helper.dll".to_owned(),
            symbol: ExportSymbol::Name("Open".to_owned()),
        };
        let _ = info
            .forwarded_exports
            .insert(ExportSymbol::Name("xlAutoOpen".to_owned()), forwarded);
        let error = verify_xll_exports(&info, path, &[]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("forwards required XLL export xlAutoOpen")
        );
    }

    #[test]
    fn pe_image_characteristics_require_an_executable_dll() {
        let path = Path::new("addin.xll");
        let mut info = xll_info();
        info.characteristics = IMAGE_FILE_EXECUTABLE_IMAGE;
        assert!(verify_image_characteristics(&info, path).is_err());
        info.characteristics = IMAGE_FILE_DLL;
        assert!(verify_image_characteristics(&info, path).is_err());
        info.characteristics = IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL | IMAGE_FILE_SYSTEM;
        assert!(verify_image_characteristics(&info, path).is_err());
        info.characteristics = IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL;
        assert!(verify_image_characteristics(&info, path).is_ok());
    }

    #[test]
    fn xll_verification_rejects_named_exports_without_executable_targets() {
        let path = Path::new("addin.xll");
        let mut info = xll_info();
        info.executable_exports.remove("xlAutoOpen");
        let error = verify_xll_exports(&info, path, &[]).unwrap_err();
        assert!(error.to_string().contains("direct executable target"));
    }

    #[test]
    fn unsafe_bundle_paths_are_rejected() {
        assert!(validate_relative("file", "../Vendor.dll").is_err());
        assert!(validate_relative("file", "/Vendor.dll").is_err());
    }

    #[test]
    fn bundle_metadata_rejects_unknown_fields() {
        let parsed: BundleMetadata =
            serde_json::from_str(
                r#"{"x86":["vendor/x86/A.dll"],"x64":["vendor/x64/A.dll"],"external-imports":["Inbox.dll"],"strict-paths":true}"#,
            )
            .unwrap();
        assert_eq!(parsed.x86.len(), 1);
        assert_eq!(parsed.external_imports, vec!["Inbox.dll"]);
        assert!(parsed.strict_paths);
        assert!(serde_json::from_str::<BundleMetadata>(r#"{"unknown":true}"#).is_err());
    }

    #[test]
    fn bundle_metadata_and_simple_resolution_default_to_strict_paths() {
        let parsed: BundleMetadata = serde_json::from_str(r#"{"x86":[],"x64":[]}"#).unwrap();
        assert!(parsed.strict_paths);
        assert!(BundleMetadata::default().strict_paths);

        let manifest = tempfile::tempdir().unwrap();
        fs::write(manifest.path().join("Engine.dll"), []).unwrap();
        let bundle = resolve_bundle_files(
            manifest.path(),
            "x86_64-pc-windows-msvc",
            &["Engine.dll".to_owned()],
        )
        .unwrap();
        assert_eq!(bundle.resolved_files().count(), 1);
    }

    #[test]
    fn case_insensitive_bundle_basenames_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("one")).unwrap();
        fs::create_dir_all(directory.path().join("two")).unwrap();
        fs::write(directory.path().join("one/Engine.dll"), []).unwrap();
        fs::write(directory.path().join("two/engine.DLL"), []).unwrap();
        let error = resolve_bundle_files(
            directory.path(),
            "x86_64-pc-windows-msvc",
            &["one/Engine.dll".to_owned(), "two/engine.DLL".to_owned()],
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("duplicate bundle basename"));
    }

    #[test]
    fn windows_invalid_and_non_ascii_dll_basenames_are_rejected_on_every_host() {
        for name in [
            "CON.dll",
            "aux.DLL",
            "COM1.dll",
            "LPT9.dll",
            "trailing.dll.",
            "trailing.dll ",
            "foo:bar.dll",
            r"foo\bar.dll",
            "Ä.dll",
        ] {
            assert!(
                windows_dll_name_key("bundle file", name).is_err(),
                "{name:?} unexpectedly passed Windows basename validation"
            );
        }
        assert_eq!(
            windows_dll_name_key("bundle file", "Engine.DLL").unwrap(),
            "engine.dll"
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn bundle_resolution_applies_windows_basename_rules_on_other_hosts() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("CON.dll"), []).unwrap();

        let error = resolve_bundle_files(
            directory.path(),
            "x86_64-pc-windows-msvc",
            &["CON.dll".to_owned()],
        )
        .unwrap_err();
        assert!(error.to_string().contains("reserved Windows device name"));
    }

    #[test]
    fn staging_flattens_multiple_configured_files() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        fs::write(source.path().join("Engine.dll"), b"engine").unwrap();
        fs::write(source.path().join("Math.dll"), b"math").unwrap();
        let bundle = resolve_bundle_files(
            source.path(),
            "x86_64-pc-windows-msvc",
            &["Engine.dll".to_owned(), "Math.dll".to_owned()],
        )
        .unwrap();
        let staging_dir = destination.path().join("stage");
        let staged = stage_bundle(&bundle, &staging_dir).unwrap();
        assert_eq!(staged.files.len(), 2);
        assert!(
            staged
                .files
                .iter()
                .all(|file| file.source.starts_with(&staging_dir))
        );
        fs::write(source.path().join("Engine.dll"), b"replaced").unwrap();
        assert_eq!(fs::read(staging_dir.join("Engine.dll")).unwrap(), b"engine");
        assert_eq!(fs::read(staging_dir.join("Math.dll")).unwrap(), b"math");
    }

    #[test]
    fn arbitrary_bytes_never_panic_the_pe_parser() {
        for length in 0..512 {
            let bytes = (0..length)
                .map(|index| ((index * 31 + length * 17) & 0xff) as u8)
                .collect::<Vec<_>>();
            let result = std::panic::catch_unwind(|| parse_pe_bytes(&bytes));
            assert!(result.is_ok(), "parser panicked for {length} bytes");
        }
    }

    fn minimal_pe(machine: u16, characteristics: u16) -> Vec<u8> {
        let peoff = 0x80usize;
        let raw = 0x200usize;
        let mut buf = vec![0_u8; 0x400];
        buf[0..2].copy_from_slice(b"MZ");
        buf[0x3c..0x40].copy_from_slice(&(peoff as u32).to_le_bytes());
        let mut offset = peoff;
        buf[offset..offset + 4].copy_from_slice(b"PE\0\0");
        offset += 4;
        buf[offset..offset + 2].copy_from_slice(&machine.to_le_bytes());
        buf[offset + 2..offset + 4].copy_from_slice(&1_u16.to_le_bytes());
        buf[offset + 16..offset + 18].copy_from_slice(&0x00f0_u16.to_le_bytes());
        buf[offset + 18..offset + 20].copy_from_slice(&characteristics.to_le_bytes());
        offset += 20;
        let optional = offset;
        buf[optional..optional + 2].copy_from_slice(&0x20b_u16.to_le_bytes());
        buf[optional + 2] = 14;
        buf[optional + 8..optional + 12].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[optional + 20..optional + 24].copy_from_slice(&0x1000_u32.to_le_bytes());
        buf[optional + 24..optional + 32].copy_from_slice(&0x140000000_u64.to_le_bytes());
        buf[optional + 32..optional + 36].copy_from_slice(&0x1000_u32.to_le_bytes());
        buf[optional + 36..optional + 40].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[optional + 40..optional + 42].copy_from_slice(&6_u16.to_le_bytes());
        buf[optional + 48..optional + 50].copy_from_slice(&6_u16.to_le_bytes());
        buf[optional + 56..optional + 60].copy_from_slice(&0x2000_u32.to_le_bytes());
        buf[optional + 60..optional + 64].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[optional + 68..optional + 70].copy_from_slice(&2_u16.to_le_bytes());
        buf[optional + 70..optional + 72].copy_from_slice(&0x8160_u16.to_le_bytes());
        buf[optional + 72..optional + 80].copy_from_slice(&0x100000_u64.to_le_bytes());
        buf[optional + 80..optional + 88].copy_from_slice(&0x1000_u64.to_le_bytes());
        buf[optional + 88..optional + 96].copy_from_slice(&0x100000_u64.to_le_bytes());
        buf[optional + 96..optional + 104].copy_from_slice(&0x1000_u64.to_le_bytes());
        buf[optional + 108..optional + 112].copy_from_slice(&16_u32.to_le_bytes());

        offset = optional + 0xf0;
        buf[offset..offset + 8].copy_from_slice(b".text\0\0\0");
        buf[offset + 8..offset + 12].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[offset + 12..offset + 16].copy_from_slice(&0x1000_u32.to_le_bytes());
        buf[offset + 16..offset + 20].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[offset + 20..offset + 24].copy_from_slice(&(raw as u32).to_le_bytes());
        buf[offset + 32..offset + 36].copy_from_slice(&0x60000020_u32.to_le_bytes());
        buf
    }

    #[test]
    fn verify_bundle_files_checks_dll_characteristics() {
        let cases = [
            (IMAGE_FILE_DLL, "not an executable PE image"),
            (IMAGE_FILE_EXECUTABLE_IMAGE, "not marked as a PE DLL"),
            (
                IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL | IMAGE_FILE_SYSTEM,
                "marked as a system image",
            ),
        ];
        for (index, (characteristics, expected)) in cases.into_iter().enumerate() {
            let directory = tempfile::tempdir().unwrap();
            let name = format!("Engine{index}.dll");
            fs::write(
                directory.path().join(&name),
                minimal_pe(Architecture::X64.machine(), characteristics),
            )
            .unwrap();
            let bundle = resolve_bundle_files(
                directory.path(),
                "x86_64-pc-windows-msvc",
                std::slice::from_ref(&name),
            )
            .unwrap();
            let error = verify_bundle_files(&bundle, "x86_64-pc-windows-msvc").unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    enum SyntheticExportTarget<'a> {
        Zero,
        Direct,
        ForwardedOrdinal(&'a str, u32),
    }

    fn synthetic_export_pe(
        ordinal_base: u32,
        targets: &[SyntheticExportTarget<'_>],
        names: &[(usize, &str)],
    ) -> Vec<u8> {
        let peoff = 0x80usize;
        let edata_raw = 0x200usize;
        let text_raw = 0x400usize;
        let mut buf = vec![0_u8; 0x600];
        buf[0..2].copy_from_slice(b"MZ");
        buf[0x3c..0x40].copy_from_slice(&(peoff as u32).to_le_bytes());
        let mut offset = peoff;
        buf[offset..offset + 4].copy_from_slice(b"PE\0\0");
        offset += 4;
        buf[offset..offset + 2].copy_from_slice(&Architecture::X64.machine().to_le_bytes());
        buf[offset + 2..offset + 4].copy_from_slice(&2_u16.to_le_bytes());
        buf[offset + 16..offset + 18].copy_from_slice(&0x00f0_u16.to_le_bytes());
        buf[offset + 18..offset + 20]
            .copy_from_slice(&(IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL).to_le_bytes());
        offset += 20;
        let optional = offset;
        buf[optional..optional + 2].copy_from_slice(&0x20b_u16.to_le_bytes());
        buf[optional + 2] = 14;
        buf[optional + 8..optional + 12].copy_from_slice(&0x400_u32.to_le_bytes());
        buf[optional + 20..optional + 24].copy_from_slice(&0x1000_u32.to_le_bytes());
        buf[optional + 24..optional + 32].copy_from_slice(&0x140000000_u64.to_le_bytes());
        buf[optional + 32..optional + 36].copy_from_slice(&0x1000_u32.to_le_bytes());
        buf[optional + 36..optional + 40].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[optional + 40..optional + 42].copy_from_slice(&6_u16.to_le_bytes());
        buf[optional + 48..optional + 50].copy_from_slice(&6_u16.to_le_bytes());
        buf[optional + 56..optional + 60].copy_from_slice(&0x3000_u32.to_le_bytes());
        buf[optional + 60..optional + 64].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[optional + 68..optional + 70].copy_from_slice(&2_u16.to_le_bytes());
        buf[optional + 70..optional + 72].copy_from_slice(&0x8160_u16.to_le_bytes());
        buf[optional + 72..optional + 80].copy_from_slice(&0x100000_u64.to_le_bytes());
        buf[optional + 80..optional + 88].copy_from_slice(&0x1000_u64.to_le_bytes());
        buf[optional + 88..optional + 96].copy_from_slice(&0x100000_u64.to_le_bytes());
        buf[optional + 96..optional + 104].copy_from_slice(&0x1000_u64.to_le_bytes());
        buf[optional + 108..optional + 112].copy_from_slice(&16_u32.to_le_bytes());
        buf[optional + 0x70..optional + 0x74].copy_from_slice(&0x1000_u32.to_le_bytes());
        buf[optional + 0x74..optional + 0x78].copy_from_slice(&0x180_u32.to_le_bytes());

        offset = optional + 0xf0;
        buf[offset..offset + 8].copy_from_slice(b".edata\0\0");
        buf[offset + 8..offset + 12].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[offset + 12..offset + 16].copy_from_slice(&0x1000_u32.to_le_bytes());
        buf[offset + 16..offset + 20].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[offset + 20..offset + 24].copy_from_slice(&(edata_raw as u32).to_le_bytes());
        buf[offset + 32..offset + 36].copy_from_slice(&0x40000040_u32.to_le_bytes());
        offset += 40;
        buf[offset..offset + 8].copy_from_slice(b".text\0\0\0");
        buf[offset + 8..offset + 12].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[offset + 12..offset + 16].copy_from_slice(&0x2000_u32.to_le_bytes());
        buf[offset + 16..offset + 20].copy_from_slice(&0x200_u32.to_le_bytes());
        buf[offset + 20..offset + 24].copy_from_slice(&(text_raw as u32).to_le_bytes());
        buf[offset + 32..offset + 36].copy_from_slice(&0x60000020_u32.to_le_bytes());

        let export = edata_raw;
        buf[export + 12..export + 16].copy_from_slice(&0x1080_u32.to_le_bytes());
        buf[export + 16..export + 20].copy_from_slice(&ordinal_base.to_le_bytes());
        buf[export + 20..export + 24].copy_from_slice(&(targets.len() as u32).to_le_bytes());
        buf[export + 24..export + 28].copy_from_slice(&(names.len() as u32).to_le_bytes());
        buf[export + 28..export + 32].copy_from_slice(&0x1030_u32.to_le_bytes());
        buf[export + 32..export + 36].copy_from_slice(&0x1040_u32.to_le_bytes());
        buf[export + 36..export + 40].copy_from_slice(&0x1050_u32.to_le_bytes());

        let mut string_offset = 0x80usize;
        buf[export + string_offset..export + string_offset + 8].copy_from_slice(b"engine\0\0");
        string_offset += 0x10;
        for (index, target) in targets.iter().enumerate() {
            let target_rva = match target {
                SyntheticExportTarget::Zero => 0,
                SyntheticExportTarget::Direct => 0x2000,
                SyntheticExportTarget::ForwardedOrdinal(library, ordinal) => {
                    let forward = format!("{library}.#{ordinal}");
                    let bytes = forward.as_bytes();
                    buf[export + string_offset..export + string_offset + bytes.len()]
                        .copy_from_slice(bytes);
                    buf[export + string_offset + bytes.len()] = 0;
                    let rva = 0x1000 + string_offset as u32;
                    string_offset += bytes.len() + 1;
                    rva
                }
            };
            let eat = export + 0x30 + index * 4;
            buf[eat..eat + 4].copy_from_slice(&target_rva.to_le_bytes());
        }
        let mut name_string_offset = string_offset.max(0x100);
        for (name_position, (index, name)) in names.iter().enumerate() {
            let bytes = name.as_bytes();
            buf[export + name_string_offset..export + name_string_offset + bytes.len()]
                .copy_from_slice(bytes);
            buf[export + name_string_offset + bytes.len()] = 0;
            let name_rva = 0x1000 + name_string_offset as u32;
            let pointer = export + 0x40 + name_position * 4;
            buf[pointer..pointer + 4].copy_from_slice(&name_rva.to_le_bytes());
            let ordinal_pointer = export + 0x50 + name_position * 2;
            buf[ordinal_pointer..ordinal_pointer + 2]
                .copy_from_slice(&(*index as u16).to_le_bytes());
            name_string_offset += bytes.len() + 1;
        }
        buf
    }

    #[test]
    fn export_validation_tracks_eat_slots_for_aliases_and_forwarders() {
        let alias = synthetic_export_pe(
            1,
            &[
                SyntheticExportTarget::Direct,
                SyntheticExportTarget::Direct,
                SyntheticExportTarget::Zero,
            ],
            &[(0, "AliasA"), (0, "AliasB")],
        );
        let info = parse_pe_bytes(&alias).unwrap();
        assert_eq!(info.exported_ordinals, BTreeSet::from([1, 2]));
        assert_eq!(info.nonzero_export_slots, BTreeSet::from([0, 1]));
        assert_eq!(info.named_export_slots, BTreeSet::from([0]));
        assert_eq!(
            info.nonzero_export_slots
                .difference(&info.named_export_slots)
                .copied()
                .collect::<Vec<_>>(),
            vec![1]
        );

        let forwarded = synthetic_export_pe(
            1,
            &[SyntheticExportTarget::ForwardedOrdinal("engine", 7)],
            &[(0, "Forwarded")],
        );
        let info = parse_pe_bytes(&forwarded).unwrap();
        assert_eq!(info.exported_ordinals, BTreeSet::from([1]));
        assert_eq!(
            info.forwarded_exports.get(&ExportSymbol::Ordinal(1)),
            Some(&ForwardedExport {
                library: "engine.dll".to_owned(),
                symbol: ExportSymbol::Ordinal(7),
            })
        );
    }

    #[test]
    fn export_ordinal_overflow_is_rejected_instead_of_dropped() {
        let bytes = synthetic_export_pe(0x1_0000, &[SyntheticExportTarget::Direct], &[]);
        let error = parse_pe_bytes(&bytes).unwrap_err();
        assert!(error.to_string().contains("exceeds u16"));
    }

    fn graph_image(imports: &[&str], delay_imports: &[&str]) -> PeInfo {
        PeInfo {
            machine: Architecture::X64.machine(),
            characteristics: IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL,
            exports: BTreeSet::new(),
            forwarded_exports: BTreeMap::new(),
            executable_exports: BTreeSet::new(),
            has_export_manifest: false,
            expected_exports: BTreeSet::new(),
            crt_policy: None,
            imports: imports.iter().map(|name| (*name).to_owned()).collect(),
            import_targets: BTreeMap::new(),
            delay_imports: delay_imports
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            delay_import_targets: BTreeMap::new(),
            exported_ordinals: BTreeSet::new(),
            nonzero_export_slots: BTreeSet::new(),
            named_export_slots: BTreeSet::new(),
        }
    }

    fn add_direct_export(image: &mut PeInfo, name: &str, ordinal: u16) {
        image.exports.insert(name.to_owned());
        image.exported_ordinals.insert(ordinal);
    }

    fn add_forwarded_export(
        image: &mut PeInfo,
        name: &str,
        ordinal: u16,
        library: &str,
        target: ExportSymbol,
    ) {
        add_direct_export(image, name, ordinal);
        let forwarded = ForwardedExport {
            library: library.to_ascii_lowercase(),
            symbol: target,
        };
        let _ = image
            .forwarded_exports
            .insert(ExportSymbol::Name(name.to_owned()), forwarded.clone());
        let _ = image
            .forwarded_exports
            .insert(ExportSymbol::Ordinal(ordinal), forwarded);
    }

    #[test]
    fn dependency_graph_reports_the_full_missing_chain() {
        let images = BTreeMap::from([
            (
                "addin.xll".to_owned(),
                ("Addin.xll".to_owned(), graph_image(&["Engine.dll"], &[])),
            ),
            (
                "engine.dll".to_owned(),
                ("Engine.dll".to_owned(), graph_image(&[], &["Model.dll"])),
            ),
        ]);
        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Addin.xll -> Engine.dll -> model.dll")
        );
    }

    #[test]
    fn dependency_graph_rejects_missing_imported_symbol() {
        let mut addin = graph_image(&["Engine.dll"], &[]);
        addin.import_targets.insert(
            "Engine.dll".to_owned(),
            BTreeSet::from([ImportTarget::Name("SymbolV2".to_owned())]),
        );
        let mut engine = graph_image(&[], &[]);
        engine.exports.insert("SymbolV1".to_owned());
        let images = BTreeMap::from([
            ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
            ("engine.dll".to_owned(), ("Engine.dll".to_owned(), engine)),
        ]);

        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(error.to_string().contains("Engine.dll!SymbolV2"));
    }

    #[test]
    fn dependency_graph_rejects_missing_imported_ordinal() {
        let mut addin = graph_image(&[], &["Engine.dll"]);
        addin.delay_import_targets.insert(
            "Engine.dll".to_owned(),
            BTreeSet::from([ImportTarget::Ordinal(17)]),
        );
        let mut engine = graph_image(&[], &[]);
        engine.exported_ordinals.insert(16);
        let images = BTreeMap::from([
            ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
            ("engine.dll".to_owned(), ("Engine.dll".to_owned(), engine)),
        ]);

        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(error.to_string().contains("Engine.dll!#17"));
    }

    #[test]
    fn dependency_graph_accepts_existing_name_and_ordinal_targets() {
        let mut addin = graph_image(&["Engine.dll"], &[]);
        addin.import_targets.insert(
            "Engine.dll".to_owned(),
            BTreeSet::from([
                ImportTarget::Name("Process".to_owned()),
                ImportTarget::Ordinal(17),
            ]),
        );
        let mut engine = graph_image(&[], &[]);
        engine.exports.insert("Process".to_owned());
        engine.exported_ordinals.insert(17);
        let images = BTreeMap::from([
            ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
            ("engine.dll".to_owned(), ("Engine.dll".to_owned(), engine)),
        ]);

        validate_dependency_graph(&images, &BTreeSet::new()).unwrap();
    }

    #[test]
    fn dynamically_linked_msvc_runtime_is_not_treated_as_an_inbox_dll() {
        let images = BTreeMap::from([(
            "addin.xll".to_owned(),
            (
                "Addin.xll".to_owned(),
                graph_image(&["vcruntime140.dll"], &[]),
            ),
        )]);
        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(error.to_string().contains("vcruntime140.dll"));
    }

    #[test]
    fn explicit_external_import_permits_dynamic_msvc_runtime() {
        let mut bundle = StagedBundle {
            files: vec![],
            external_imports: BTreeSet::new(),
        };
        bundle
            .try_add_external_imports(["vcruntime140.dll"])
            .unwrap();
        let images = BTreeMap::from([(
            "addin.xll".to_owned(),
            (
                "Addin.xll".to_owned(),
                graph_image(&["vcruntime140.dll"], &[]),
            ),
        )]);
        assert!(validate_dependency_graph(&images, &bundle.external_imports).is_ok());
    }

    #[test]
    fn staged_bundle_external_imports_validate_all_inputs_before_mutating() {
        let mut bundle = StagedBundle {
            files: vec![],
            external_imports: BTreeSet::new(),
        };
        let error = bundle
            .try_add_external_imports(["trusted.dll", "vendor/not-a-basename.dll"])
            .unwrap_err();
        assert!(error.to_string().contains("external import"));
        assert!(bundle.external_imports.is_empty());
    }

    #[test]
    fn api_set_looking_names_are_not_implicitly_trusted() {
        let images = BTreeMap::from([(
            "addin.xll".to_owned(),
            (
                "Addin.xll".to_owned(),
                graph_image(&["api-ms-win-not-a-real-contract.dll"], &[]),
            ),
        )]);
        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("api-ms-win-not-a-real-contract.dll")
        );

        let external =
            normalize_external_imports(&["api-ms-win-not-a-real-contract.dll".to_owned()]).unwrap();
        validate_dependency_graph(&images, &external).unwrap();
    }

    #[test]
    fn explicit_external_import_is_accepted_and_validated_as_a_dll_basename() {
        let images = BTreeMap::from([(
            "addin.xll".to_owned(),
            (
                "Addin.xll".to_owned(),
                graph_image(&["Some-Inbox-Component.dll"], &[]),
            ),
        )]);
        let external =
            normalize_external_imports(&["some-inbox-component.DLL".to_owned()]).unwrap();
        validate_dependency_graph(&images, &external).unwrap();
        assert!(normalize_external_imports(&["directory/vendor.dll".to_owned()]).is_err());
        assert!(normalize_external_imports(&["directory\\vendor.dll".to_owned()]).is_err());
        assert!(normalize_external_imports(&["..\\vendor.dll".to_owned()]).is_err());
        assert!(normalize_external_imports(&["C:\\Temp\\vendor.dll".to_owned()]).is_err());
        assert!(normalize_external_imports(&["vendor.exe".to_owned()]).is_err());
    }

    #[test]
    fn dependency_graph_rejects_missing_forwarded_library() {
        let mut addin = graph_image(&[], &[]);
        add_forwarded_export(
            &mut addin,
            "Process",
            1,
            "engine.dll",
            ExportSymbol::Name("ProcessImpl".to_owned()),
        );
        let images = BTreeMap::from([("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin))]);
        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(error.to_string().contains("engine.dll"));
    }

    #[test]
    fn dependency_graph_validates_forwarded_symbol_and_chain() {
        let mut addin = graph_image(&[], &[]);
        add_forwarded_export(
            &mut addin,
            "Process",
            1,
            "engine.dll",
            ExportSymbol::Name("ProcessImpl".to_owned()),
        );
        let mut engine = graph_image(&[], &[]);
        add_forwarded_export(
            &mut engine,
            "ProcessImpl",
            2,
            "model.dll",
            ExportSymbol::Ordinal(7),
        );
        let mut model = graph_image(&[], &[]);
        model.exported_ordinals.insert(7);
        let images = BTreeMap::from([
            ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
            ("engine.dll".to_owned(), ("Engine.dll".to_owned(), engine)),
            ("model.dll".to_owned(), ("Model.dll".to_owned(), model)),
        ]);
        validate_dependency_graph(&images, &BTreeSet::new()).unwrap();
    }

    #[test]
    fn dependency_graph_rejects_missing_forwarded_symbol() {
        let mut addin = graph_image(&[], &[]);
        add_forwarded_export(
            &mut addin,
            "Process",
            1,
            "engine.dll",
            ExportSymbol::Name("Missing".to_owned()),
        );
        let images = BTreeMap::from([
            ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
            (
                "engine.dll".to_owned(),
                ("Engine.dll".to_owned(), graph_image(&[], &[])),
            ),
        ]);
        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("forwarded export target is missing")
        );
    }

    #[test]
    fn dependency_graph_rejects_forwarder_cycles() {
        let mut addin = graph_image(&[], &[]);
        add_forwarded_export(
            &mut addin,
            "Process",
            1,
            "engine.dll",
            ExportSymbol::Name("ProcessImpl".to_owned()),
        );
        let mut engine = graph_image(&[], &[]);
        add_forwarded_export(
            &mut engine,
            "ProcessImpl",
            2,
            "addin.xll",
            ExportSymbol::Name("Process".to_owned()),
        );
        let images = BTreeMap::from([
            ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
            ("engine.dll".to_owned(), ("Engine.dll".to_owned(), engine)),
        ]);
        let error = validate_dependency_graph(&images, &BTreeSet::new()).unwrap_err();
        assert!(error.to_string().contains("cyclic forwarded export"));
    }

    #[cfg(unix)]
    #[test]
    fn bundle_rejects_symlink_escape_and_strict_mode_rejects_any_symlink() {
        use std::os::unix::fs::symlink;

        let manifest = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("Vendor.dll"), b"outside").unwrap();
        symlink(
            outside.path().join("Vendor.dll"),
            manifest.path().join("Escape.dll"),
        )
        .unwrap();
        let strict_error = resolve_bundle_files(
            manifest.path(),
            "x86_64-pc-windows-msvc",
            &["Escape.dll".to_owned()],
        )
        .unwrap_err();
        assert!(strict_error.to_string().contains("rejects symlink"));

        let relaxed_escape_error = resolve_bundle_files_with_policy(
            manifest.path(),
            "x86_64-pc-windows-msvc",
            &["Escape.dll".to_owned()],
            &[],
            false,
        )
        .unwrap_err();
        assert!(
            relaxed_escape_error
                .to_string()
                .contains("escapes manifest directory")
        );

        fs::write(manifest.path().join("Inside.dll"), b"inside").unwrap();
        symlink(
            manifest.path().join("Inside.dll"),
            manifest.path().join("Alias.dll"),
        )
        .unwrap();
        let relaxed = resolve_bundle_files_with_policy(
            manifest.path(),
            "x86_64-pc-windows-msvc",
            &["Alias.dll".to_owned()],
            &[],
            false,
        )
        .unwrap();
        assert_eq!(relaxed.resolved_files().count(), 1);
        let strict = resolve_bundle_files_with_policy(
            manifest.path(),
            "x86_64-pc-windows-msvc",
            &["Alias.dll".to_owned()],
            &[],
            true,
        )
        .unwrap_err();
        assert!(strict.to_string().contains("rejects symlink"));
    }

    #[test]
    fn stage_bundle_rejects_existing_destination() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        fs::write(source.path().join("Engine.dll"), b"engine").unwrap();
        let bundle = resolve_bundle_files(
            source.path(),
            "x86_64-pc-windows-msvc",
            &["Engine.dll".to_owned()],
        )
        .unwrap();
        let error = stage_bundle(&bundle, destination.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("staging destination already exists")
        );
    }

    #[cfg(any(unix, target_os = "windows"))]
    #[test]
    fn file_identity_distinguishes_replaced_sources() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.dll");
        let second = directory.path().join("second.dll");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();

        let opened = std::fs::File::open(&first).unwrap();
        let same = std::fs::File::open(&first).unwrap();
        let different = std::fs::File::open(&second).unwrap();
        assert!(same_file_identity(&opened, &same).unwrap());
        assert!(!same_file_identity(&opened, &different).unwrap());
    }

    #[cfg(target_os = "windows")]
    struct BlockingAfterOpenObserver {
        entered: std::sync::Arc<std::sync::Barrier>,
        release: std::sync::Arc<std::sync::Barrier>,
    }

    #[cfg(target_os = "windows")]
    impl SnapshotObserver for BlockingAfterOpenObserver {
        fn after_open(&self, _path: &Path) {
            self.entered.wait();
            self.release.wait();
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn snapshot_denies_in_place_writers() {
        use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("Engine.dll");
        fs::write(&source, vec![0x41; 1024 * 1024]).unwrap();

        let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release = std::sync::Arc::new(std::sync::Barrier::new(2));
        let observer = BlockingAfterOpenObserver {
            entered: std::sync::Arc::clone(&entered),
            release: std::sync::Arc::clone(&release),
        };

        let manifest_directory = directory.path().to_owned();
        let resolver = std::thread::spawn(move || {
            resolve_bundle_files_with_policy_impl(
                &manifest_directory,
                "x86_64-pc-windows-msvc",
                &["Engine.dll".to_owned()],
                &[],
                false,
                &observer,
            )
        });

        entered.wait();

        let error = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));

        release.wait();
        resolver.join().unwrap().unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn snapshot_rejects_source_already_open_for_writing() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("Engine.dll");
        fs::write(&source, b"original").unwrap();

        let _writer = std::fs::OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&source)
            .unwrap();

        let error = resolve_bundle_files(
            directory.path(),
            "x86_64-pc-windows-msvc",
            &["Engine.dll".to_owned()],
        )
        .unwrap_err();

        assert!(matches!(error, PackageError::BundleSourceBusy { .. }));
    }

    #[cfg(unix)]
    struct BlockingAfterFirstChunkObserver {
        entered: std::sync::Arc<std::sync::Barrier>,
        release: std::sync::Arc<std::sync::Barrier>,
    }

    #[cfg(unix)]
    impl SnapshotObserver for BlockingAfterFirstChunkObserver {
        fn after_first_chunk(&self, _path: &Path) {
            self.entered.wait();
            self.release.wait();
        }
    }

    #[cfg(unix)]
    #[test]
    fn same_length_in_place_mutation_during_snapshot_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("Engine.dll");
        let initial = vec![0x11; 4 * 1024 * 1024];
        let replacement = vec![0x22; initial.len()];
        fs::write(&source, &initial).unwrap();

        let entered = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release = std::sync::Arc::new(std::sync::Barrier::new(2));
        let observer = BlockingAfterFirstChunkObserver {
            entered: std::sync::Arc::clone(&entered),
            release: std::sync::Arc::clone(&release),
        };

        let manifest_directory = directory.path().to_owned();
        let resolver = std::thread::spawn(move || {
            resolve_bundle_files_with_policy_impl(
                &manifest_directory,
                "x86_64-pc-windows-msvc",
                &["Engine.dll".to_owned()],
                &[],
                false,
                &observer,
            )
        });

        entered.wait();
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap();
        writer.write_all(&replacement).unwrap();
        writer.flush().unwrap();
        drop(writer);
        release.wait();

        let error = resolver.join().unwrap().unwrap_err();
        assert!(matches!(error, PackageError::UnstableBundleSource { .. }));
    }

    #[test]
    fn stage_bundle_uses_the_resolved_file_snapshot() {
        let source = tempfile::tempdir().unwrap();
        let source_path = source.path().join("Engine.dll");
        fs::write(&source_path, b"resolved bytes").unwrap();
        let bundle = resolve_bundle_files(
            source.path(),
            "x86_64-pc-windows-msvc",
            &["Engine.dll".to_owned()],
        )
        .unwrap();

        fs::write(&source_path, b"replacement bytes").unwrap();
        let destination = source.path().join("staging");
        stage_bundle(&bundle, &destination).unwrap();

        assert_eq!(
            fs::read(destination.join("Engine.dll")).unwrap(),
            b"resolved bytes"
        );
    }

    #[test]
    fn bundle_rejects_windows_system_dll_name_collisions() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("version.dll"), b"not the system DLL").unwrap();

        let error = resolve_bundle_files(
            source.path(),
            "x86_64-pc-windows-msvc",
            &["version.dll".to_owned()],
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("must not shadow Windows system DLL")
        );
    }

    #[test]
    fn va_based_delay_load_import_descriptor_is_rejected() {
        let peoff = 0x80usize;
        let raw = 0x200usize;
        let section_rva = 0x1000u32;
        let mut buf = vec![0u8; 0x400];
        buf[0..2].copy_from_slice(b"MZ");
        buf[0x3c..0x40].copy_from_slice(&(peoff as u32).to_le_bytes());
        let mut o = peoff;
        buf[o..o + 4].copy_from_slice(b"PE\0\0");
        o += 4;
        buf[o..o + 2].copy_from_slice(&0x8664u16.to_le_bytes());
        buf[o + 2..o + 4].copy_from_slice(&1u16.to_le_bytes());
        buf[o + 16..o + 18].copy_from_slice(&0x00f0u16.to_le_bytes());
        buf[o + 18..o + 20].copy_from_slice(&0x2022u16.to_le_bytes());
        o += 20;
        let opt = o;
        buf[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        buf[opt + 2] = 14;
        buf[opt + 8..opt + 12].copy_from_slice(&0x200u32.to_le_bytes());
        buf[opt + 20..opt + 24].copy_from_slice(&0x1000u32.to_le_bytes());
        buf[opt + 24..opt + 32].copy_from_slice(&0x10000u64.to_le_bytes());
        buf[opt + 32..opt + 36].copy_from_slice(&0x1000u32.to_le_bytes());
        buf[opt + 36..opt + 40].copy_from_slice(&0x200u32.to_le_bytes());
        buf[opt + 40..opt + 42].copy_from_slice(&6u16.to_le_bytes());
        buf[opt + 48..opt + 50].copy_from_slice(&6u16.to_le_bytes());
        buf[opt + 56..opt + 60].copy_from_slice(&0x2000u32.to_le_bytes());
        buf[opt + 60..opt + 64].copy_from_slice(&0x200u32.to_le_bytes());
        buf[opt + 68..opt + 70].copy_from_slice(&2u16.to_le_bytes());
        buf[opt + 70..opt + 72].copy_from_slice(&0x8160u16.to_le_bytes());
        buf[opt + 72..opt + 80].copy_from_slice(&0x100000u64.to_le_bytes());
        buf[opt + 80..opt + 88].copy_from_slice(&0x1000u64.to_le_bytes());
        buf[opt + 88..opt + 96].copy_from_slice(&0x100000u64.to_le_bytes());
        buf[opt + 96..opt + 104].copy_from_slice(&0x1000u64.to_le_bytes());
        buf[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
        buf[opt + 0x70 + 13 * 8..opt + 0x70 + 13 * 8 + 4]
            .copy_from_slice(&section_rva.to_le_bytes());
        buf[opt + 0x70 + 13 * 8 + 4..opt + 0x70 + 13 * 8 + 8]
            .copy_from_slice(&0x40u32.to_le_bytes());

        o = opt + 0xf0;
        buf[o..o + 8].copy_from_slice(b".rdata\0\0");
        o += 8;
        buf[o..o + 4].copy_from_slice(&0x200u32.to_le_bytes());
        buf[o + 4..o + 8].copy_from_slice(&section_rva.to_le_bytes());
        buf[o + 8..o + 12].copy_from_slice(&0x200u32.to_le_bytes());
        buf[o + 12..o + 16].copy_from_slice(&(raw as u32).to_le_bytes());
        buf[o + 32..o + 36].copy_from_slice(&0x40000040u32.to_le_bytes());

        buf[raw..raw + 4].copy_from_slice(&0u32.to_le_bytes());
        buf[raw + 4..raw + 8].copy_from_slice(&(section_rva + 0x40).to_le_bytes());
        let name = b"evil.dll\0";
        buf[raw + 0x40..raw + 0x40 + name.len()].copy_from_slice(name);

        let error = parse_pe_bytes(&buf).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported VA-based delay load import descriptor")
        );
    }

    proptest! {
        #[test]
        fn generated_bytes_never_panic_the_pe_parser(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
            prop_assert!(std::panic::catch_unwind(|| parse_pe_bytes(&bytes)).is_ok());
        }
    }

    #[test]
    fn verify_xll_exports_rejects_unmanifested_custom_entry_and_ordinals() {
        let temp_dir = tempfile::tempdir().unwrap();
        let xll_path = temp_dir.path().join("test.xll");

        let mut info = PeInfo {
            exports: BTreeSet::from(["xlAutoOpen".to_string(), "CustomEntry".to_string()]),
            executable_exports: BTreeSet::from([
                "xlAutoOpen".to_string(),
                "CustomEntry".to_string(),
            ]),
            expected_exports: BTreeSet::from(["xlAutoOpen".to_string()]),
            ..Default::default()
        };

        assert!(verify_xll_exports(&info, &xll_path, &[]).is_err());

        info.exports.remove("CustomEntry");
        info.executable_exports.remove("CustomEntry");
        info.nonzero_export_slots.extend([1, 2]);
        assert!(verify_xll_exports(&info, &xll_path, &[]).is_err());
    }
}
