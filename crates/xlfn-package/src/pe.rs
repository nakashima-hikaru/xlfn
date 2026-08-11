use super::*;

pub fn verify_xll(path: &Path, target: &str, required_exports: &[String]) -> PackageResult {
    verify_xll_bytes(&fs::read(path)?, target, required_exports, path)
}

pub(crate) fn verify_xll_bytes(
    bytes: &[u8],
    target: &str,
    required_exports: &[String],
    path: &Path,
) -> PackageResult {
    let info = parse_pe_bytes(bytes)?;
    verify_machine(&info, Architecture::parse(target)?, path)?;
    verify_image_characteristics(&info, path)?;
    verify_xll_exports(&info, path, required_exports)
}

pub(crate) fn verify_xll_exports(
    info: &PeInfo,
    path: &Path,
    required_exports: &[String],
) -> PackageResult {
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
pub(crate) fn verify_dependency_closure(
    xll: &Path,
    target: &str,
    bundle: &StagedBundle,
    xll_snapshot: &[u8],
) -> PackageResult {
    let architecture = Architecture::parse(target)?;
    let root_name = xll
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("XLL has no UTF-8 basename: {}", xll.display()))?;
    let mut images = BTreeMap::new();
    let root_key = windows_name_key("XLL basename", root_name)?;
    images.insert(
        root_key.clone(),
        (
            root_name.to_owned(),
            inspect_checked_pe_bytes(xll_snapshot, architecture, xll)?,
        ),
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
                    inspect_checked_bundle_file(file, architecture)?,
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
        if is_system(&key) {
            return Err(format!("bundle must not shadow Windows system DLL {name:?}").into());
        }
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
    let digest = hash.finalize();
    Ok(digest_hex(&digest))
}

pub(crate) fn sha256_digest(bytes: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(bytes);
    let digest = hash.finalize();
    let mut output = [0_u8; 32];
    output.copy_from_slice(&digest);
    output
}
pub(crate) fn validate_imports(
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

pub(crate) fn inspect_checked_pe(path: &Path, architecture: Architecture) -> PackageResult<PeInfo> {
    let info = inspect_pe(path)?;
    inspect_checked_info(info, architecture, path)
}

pub(crate) fn inspect_checked_pe_bytes(
    bytes: &[u8],
    architecture: Architecture,
    path: &Path,
) -> PackageResult<PeInfo> {
    let info = parse_pe_bytes(bytes)?;
    inspect_checked_info(info, architecture, path)
}

pub(crate) fn inspect_checked_info(
    info: PeInfo,
    architecture: Architecture,
    path: &Path,
) -> PackageResult<PeInfo> {
    verify_machine(&info, architecture, path)?;
    verify_image_characteristics(&info, path)?;
    Ok(info)
}

pub(crate) fn inspect_checked_bundle_file(
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

pub(crate) fn validate_dependency_graph(
    images: &BTreeMap<String, (String, PeInfo)>,
    external_imports: &BTreeSet<String>,
) -> PackageResult {
    let bundled_names = images.keys().cloned().collect::<BTreeSet<_>>();
    if let Some(name) = external_imports.intersection(&bundled_names).next() {
        return Err(format!("DLL `{name}` cannot be both bundled and external").into());
    }
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

pub(crate) fn validate_dependency_node(
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
                    ImportTarget::Ordinal(ordinal) => imported_image
                        .exported_ordinals
                        .contains(&ExportOrdinal(*ordinal)),
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

pub(crate) fn validate_forwarded_exports(
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

pub(crate) fn validate_forwarded_symbol(
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

pub(crate) fn format_export_symbol(symbol: &ExportSymbol) -> String {
    match symbol {
        ExportSymbol::Name(name) => name.clone(),
        ExportSymbol::Ordinal(ordinal) => format!("#{ordinal}"),
    }
}

pub(crate) fn verify_machine(
    info: &PeInfo,
    architecture: Architecture,
    path: &Path,
) -> PackageResult {
    if info.machine == architecture.pe_machine() {
        Ok(())
    } else {
        Err(format!("{} has wrong PE machine", path.display()).into())
    }
}
pub(crate) fn verify_image_characteristics(info: &PeInfo, path: &Path) -> PackageResult {
    if info.characteristics & IMAGE_FILE_EXECUTABLE_IMAGE == object::pe::FileFlags::default() {
        return Err(format!("{} is not an executable PE image", path.display()).into());
    }
    if info.characteristics & IMAGE_FILE_DLL == object::pe::FileFlags::default() {
        return Err(format!("{} is not marked as a PE DLL", path.display()).into());
    }
    if info.characteristics & IMAGE_FILE_SYSTEM != object::pe::FileFlags::default() {
        return Err(format!("{} is marked as a system image", path.display()).into());
    }
    Ok(())
}

pub(crate) fn reject_forwarded_xll_export(
    info: &PeInfo,
    path: &Path,
    export: &str,
) -> PackageResult {
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
pub(crate) fn is_system(name: &str) -> bool {
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
    Ordinal(ExportOrdinal),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForwardedExport {
    pub library: String,
    pub symbol: ExportSymbol,
}

#[derive(Clone, Debug)]
pub struct PeInfo {
    pub machine: object::pe::Machine,
    pub characteristics: object::pe::FileFlags,
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
    pub exported_ordinals: BTreeSet<ExportOrdinal>,
    /// Non-zero export address table indices, used for closed-world validation.
    pub nonzero_export_slots: BTreeSet<ExportAddressIndex>,
    /// Export address table indices referenced by at least one export name.
    pub named_export_slots: BTreeSet<ExportAddressIndex>,
}

impl Default for PeInfo {
    fn default() -> Self {
        Self {
            machine: IMAGE_FILE_MACHINE_AMD64,
            characteristics: object::pe::FileFlags::default(),
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
    pub(crate) fn has_export_symbol(&self, symbol: &ExportSymbol) -> bool {
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

pub(crate) fn normalize_forwarder_library(library: &str) -> PackageResult<String> {
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

pub(crate) fn parse_pe_file<Pe>(pe: PeFile<'_, Pe>) -> PackageResult<PeInfo>
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
        let mut export_targets = BTreeMap::new();
        for (ordinal_index, exported_ordinal, address) in table.address_iter() {
            if address == 0 {
                export_targets.insert(ordinal_index, None);
                continue;
            }

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
                                "PE export ordinal {exported_ordinal} points outside every mapped section"
                            ))
                        })?;
                    (section.characteristics.get(LE) & IMAGE_SCN_MEM_EXECUTE)
                        != object::pe::SectionFlags::default()
                }
                ExportTarget::ForwardByOrdinal(library, target_ordinal) => {
                    let forwarded = ForwardedExport {
                        library: normalize_forwarder_library(std::str::from_utf8(library)?)?,
                        symbol: ExportSymbol::Ordinal(target_ordinal),
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

            export_targets.insert(ordinal_index, Some(executable));
        }

        for (name_pointer, ordinal_index) in table.name_iter() {
            named_export_slots.insert(ordinal_index);
            let target = export_targets
                .get(&ordinal_index)
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
