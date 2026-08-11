use super::*;

use proptest::prelude::*;

fn xll_info() -> PeInfo {
    let framework = REQUIRED_XLL_EXPORTS
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    PeInfo {
        machine: IMAGE_FILE_MACHINE_AMD64,
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

#[cfg(unix)]
#[test]
fn directory_validation_checks_existing_ancestors() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let real_parent = directory.path().join("real-parent");
    let linked_parent = directory.path().join("linked-parent");
    fs::create_dir(&real_parent).unwrap();
    symlink(&real_parent, &linked_parent).unwrap();

    let error = validate_directory_path(&linked_parent.join("nested").join("dist")).unwrap_err();
    assert!(error.to_string().contains("symlink or reparse point"));
}

#[test]
fn bounded_directory_entry_read_stops_before_collecting_extra_entries() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("first"), []).unwrap();
    fs::write(directory.path().join("second"), []).unwrap();

    let error = read_directory_entries(directory.path(), 1).unwrap_err();
    assert!(error.to_string().contains("entry budget"));
}

#[test]
fn bundle_metadata_rejects_unknown_fields() {
    let parsed: BundleMetadata =
        serde_json::from_str(
            r#"{"x86":["native/x86/A.dll"],"x64":["native/x64/A.dll"],"external-imports":["Inbox.dll"],"strict-paths":true}"#,
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
    let staging_directory = PrivateStagingDirectory::create(&staging_dir).unwrap();
    let staged = stage_bundle(&bundle, &staging_directory).unwrap();
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

fn write_u16<T>(destination: &mut [u8], value: T)
where
    T: object::Wrap<Inner = u16> + Copy + 'static,
{
    let encoded = object::endian::U16::<LE, T>::new(LE, value);
    destination.copy_from_slice(object::pod::bytes_of(&encoded));
}

fn minimal_pe(machine: object::pe::Machine, characteristics: object::pe::FileFlags) -> Vec<u8> {
    let peoff = 0x80usize;
    let raw = 0x200usize;
    let mut buf = vec![0_u8; 0x400];
    buf[0..2].copy_from_slice(b"MZ");
    buf[0x3c..0x40].copy_from_slice(&(peoff as u32).to_le_bytes());
    let mut offset = peoff;
    buf[offset..offset + 4].copy_from_slice(b"PE\0\0");
    offset += 4;
    write_u16(&mut buf[offset..offset + 2], machine);
    buf[offset + 2..offset + 4].copy_from_slice(&1_u16.to_le_bytes());
    buf[offset + 16..offset + 18].copy_from_slice(&0x00f0_u16.to_le_bytes());
    write_u16(&mut buf[offset + 18..offset + 20], characteristics);
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
            minimal_pe(IMAGE_FILE_MACHINE_AMD64, characteristics),
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
    write_u16(&mut buf[offset..offset + 2], IMAGE_FILE_MACHINE_AMD64);
    buf[offset + 2..offset + 4].copy_from_slice(&2_u16.to_le_bytes());
    buf[offset + 16..offset + 18].copy_from_slice(&0x00f0_u16.to_le_bytes());
    write_u16(
        &mut buf[offset + 18..offset + 20],
        IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL,
    );
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
        buf[ordinal_pointer..ordinal_pointer + 2].copy_from_slice(&(*index as u16).to_le_bytes());
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
    assert_eq!(
        info.exported_ordinals,
        BTreeSet::from([ExportOrdinal(1), ExportOrdinal(2)])
    );
    assert_eq!(
        info.nonzero_export_slots,
        BTreeSet::from([ExportAddressIndex(0), ExportAddressIndex(1)])
    );
    assert_eq!(
        info.named_export_slots,
        BTreeSet::from([ExportAddressIndex(0)])
    );
    assert_eq!(
        info.nonzero_export_slots
            .difference(&info.named_export_slots)
            .copied()
            .collect::<Vec<_>>(),
        vec![ExportAddressIndex(1)]
    );

    let forwarded = synthetic_export_pe(
        1,
        &[SyntheticExportTarget::ForwardedOrdinal("engine", 7)],
        &[(0, "Forwarded")],
    );
    let info = parse_pe_bytes(&forwarded).unwrap();
    assert_eq!(info.exported_ordinals, BTreeSet::from([ExportOrdinal(1)]));
    assert_eq!(
        info.forwarded_exports
            .get(&ExportSymbol::Ordinal(ExportOrdinal(1))),
        Some(&ForwardedExport {
            library: "engine.dll".to_owned(),
            symbol: ExportSymbol::Ordinal(ExportOrdinal(7)),
        })
    );
}

#[test]
fn export_ordinal_overflow_is_rejected_instead_of_dropped() {
    let bytes = synthetic_export_pe(0x1_0000, &[SyntheticExportTarget::Direct], &[]);
    let error = parse_pe_bytes(&bytes).unwrap_err();
    assert!(error.to_string().contains("ordinal"), "{error}");
}

fn graph_image(imports: &[&str], delay_imports: &[&str]) -> PeInfo {
    PeInfo {
        machine: IMAGE_FILE_MACHINE_AMD64,
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
    image.exported_ordinals.insert(ExportOrdinal(ordinal));
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
        .insert(ExportSymbol::Ordinal(ExportOrdinal(ordinal)), forwarded);
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
    engine.exported_ordinals.insert(ExportOrdinal(16));
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
    engine.exported_ordinals.insert(ExportOrdinal(17));
    let images = BTreeMap::from([
        ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
        ("engine.dll".to_owned(), ("Engine.dll".to_owned(), engine)),
    ]);

    validate_dependency_graph(&images, &BTreeSet::new()).unwrap();
}

#[test]
fn dependency_graph_rejects_bundled_external_collision_for_regular_import() {
    let images = BTreeMap::from([
        (
            "vendor.dll".to_owned(),
            ("Vendor.dll".to_owned(), graph_image(&[], &[])),
        ),
        (
            "addin.xll".to_owned(),
            ("Addin.xll".to_owned(), graph_image(&["Vendor.dll"], &[])),
        ),
    ]);
    let external = BTreeSet::from(["vendor.dll".to_owned()]);

    let error = validate_dependency_graph(&images, &external).unwrap_err();
    assert_eq!(
        error.to_string(),
        "DLL `vendor.dll` cannot be both bundled and external"
    );
}

#[test]
fn dependency_graph_rejects_bundled_external_collision_for_delay_import() {
    let images = BTreeMap::from([
        (
            "vendor.dll".to_owned(),
            ("Vendor.dll".to_owned(), graph_image(&[], &[])),
        ),
        (
            "addin.xll".to_owned(),
            ("Addin.xll".to_owned(), graph_image(&[], &["Vendor.dll"])),
        ),
    ]);
    let external = BTreeSet::from(["vendor.dll".to_owned()]);

    let error = validate_dependency_graph(&images, &external).unwrap_err();
    assert_eq!(
        error.to_string(),
        "DLL `vendor.dll` cannot be both bundled and external"
    );
}

#[test]
fn dependency_graph_rejects_bundled_external_collision_for_forwarded_export() {
    let mut addin = graph_image(&[], &[]);
    add_forwarded_export(
        &mut addin,
        "Process",
        1,
        "Vendor.dll",
        ExportSymbol::Name("ProcessImpl".to_owned()),
    );
    let images = BTreeMap::from([
        ("addin.xll".to_owned(), ("Addin.xll".to_owned(), addin)),
        (
            "vendor.dll".to_owned(),
            ("Vendor.dll".to_owned(), graph_image(&[], &[])),
        ),
    ]);
    let external = BTreeSet::from(["vendor.dll".to_owned()]);

    let error = validate_dependency_graph(&images, &external).unwrap_err();
    assert_eq!(
        error.to_string(),
        "DLL `vendor.dll` cannot be both bundled and external"
    );
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
    let external = normalize_external_imports(&["some-inbox-component.DLL".to_owned()]).unwrap();
    validate_dependency_graph(&images, &external).unwrap();
    assert!(normalize_external_imports(&["directory/vendor.dll".to_owned()]).is_err());
    assert!(normalize_external_imports(&["directory\\vendor.dll".to_owned()]).is_err());
    assert!(normalize_external_imports(&["..\\vendor.dll".to_owned()]).is_err());
    assert!(normalize_external_imports(&["C:\\Temp\\vendor.dll".to_owned()]).is_err());
    assert!(normalize_external_imports(&["vendor.exe".to_owned()]).is_err());
}

#[test]
fn public_dependency_verifier_rejects_a_bundled_system_dll_basename() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Addin.xll");
    let bundled = directory.path().join("version.dll");
    fs::write(
        &root,
        minimal_pe(
            IMAGE_FILE_MACHINE_AMD64,
            IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL,
        ),
    )
    .unwrap();
    fs::write(
        &bundled,
        minimal_pe(
            IMAGE_FILE_MACHINE_AMD64,
            IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_DLL,
        ),
    )
    .unwrap();

    let error = verify_pe_dependency_closure(
        &root,
        "x86_64-pc-windows-msvc",
        std::slice::from_ref(&bundled),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("must not shadow Windows system DLL")
    );
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
        ExportSymbol::Ordinal(ExportOrdinal(7)),
    );
    let mut model = graph_image(&[], &[]);
    model.exported_ordinals.insert(ExportOrdinal(7));
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
fn private_staging_directory_rejects_existing_destination() {
    let destination = tempfile::tempdir().unwrap();
    let error = PrivateStagingDirectory::create(destination.path()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("private staging directory already exists")
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
    use crate::win32::ERROR_SHARING_VIOLATION;

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
    use crate::win32::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    use std::os::windows::fs::OpenOptionsExt;

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
fn snapshot_file_keeps_the_bytes_from_its_open_handle() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("Addin.dll");
    fs::write(&source, b"original bytes").unwrap();

    let snapshot = snapshot_file("x86_64-pc-windows-msvc", &source).unwrap();
    fs::write(&source, b"replacement bytes").unwrap();

    assert_eq!(snapshot.as_ref(), b"original bytes");
}

#[test]
fn verified_artifacts_keep_bytes_and_identity_for_commit_checks() {
    let directory = tempfile::tempdir().unwrap();
    let staged = directory.path().join("Engine.dll");
    fs::write(&staged, b"stable bytes").unwrap();
    let bytes: Arc<[u8]> = Arc::from(&b"stable bytes"[..]);
    let artifact = verified_artifact(
        PathBuf::from("Engine.dll"),
        bytes,
        fs::metadata(&staged).unwrap().permissions(),
    );
    let manifest = serde_json::to_vec(&serde_json::json!({
        "files": [{
            "relative_path": "Engine.dll",
            "size": artifact.size(),
            "sha256": artifact.sha256_hex(),
        }]
    }))
    .unwrap();
    let package = VerifiedPackage {
        artifacts: vec![artifact],
        expected_names: BTreeSet::from(["engine.dll".to_owned()]),
    }
    .with_manifest_bytes(manifest.clone())
    .unwrap();
    fs::write(directory.path().join("build-manifest.json"), &manifest).unwrap();

    let artifact = &package.artifacts()[0];
    assert_eq!(artifact.relative_path(), Path::new("Engine.dll"));
    assert_eq!(artifact.bytes(), b"stable bytes");
    assert_eq!(artifact.size(), 12);
    assert_eq!(
        artifact.sha256_hex(),
        "3821461753e58afa7abe81ccec8ea5ac178ea27ee92ede53771a95a101928e40"
    );
    let prepared = package
        .prepare_commit(directory.path(), "x86_64-pc-windows-msvc")
        .unwrap();
    prepared.verify_source_contents().unwrap();

    let rebuilt_parent = tempfile::tempdir().unwrap();
    let rebuilt = rebuilt_parent.path().join("rebuilt");
    let rebuilt_directory = PrivateStagingDirectory::create(&rebuilt).unwrap();
    package.materialize(&rebuilt_directory).unwrap();
    assert_eq!(
        fs::read(rebuilt.join("Engine.dll")).unwrap(),
        b"stable bytes"
    );

    fs::write(&staged, b"changed bytes").unwrap();
    assert!(
        package
            .prepare_commit(directory.path(), "x86_64-pc-windows-msvc")
            .is_err()
    );
}

#[test]
fn prepared_package_rejects_unknown_entries_and_manifest_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let staged = directory.path().join("Engine.dll");
    fs::write(&staged, b"stable bytes").unwrap();
    let artifact = verified_artifact(
        PathBuf::from("Engine.dll"),
        Arc::from(&b"stable bytes"[..]),
        fs::metadata(&staged).unwrap().permissions(),
    );
    let manifest = serde_json::to_vec(&serde_json::json!({
        "files": [{
            "relative_path": "Engine.dll",
            "size": artifact.size(),
            "sha256": artifact.sha256_hex(),
        }]
    }))
    .unwrap();
    let package = VerifiedPackage {
        artifacts: vec![artifact],
        expected_names: BTreeSet::from(["engine.dll".to_owned()]),
    }
    .with_manifest_bytes(manifest.clone())
    .unwrap();
    fs::write(directory.path().join("build-manifest.json"), manifest).unwrap();
    let prepared = package
        .prepare_commit(directory.path(), "x86_64-pc-windows-msvc")
        .unwrap();

    fs::write(directory.path().join("version.dll"), b"shadow").unwrap();
    assert!(prepared.verify_source_contents().is_err());
    fs::remove_file(directory.path().join("version.dll")).unwrap();

    fs::write(directory.path().join("build-manifest.json"), b"{}").unwrap();
    assert!(prepared.verify_source_contents().is_err());
}

#[cfg(unix)]
#[test]
fn prepared_package_opens_entries_without_following_symlinks() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let staged = directory.path().join("Engine.dll");
    let replacement = directory.path().join("replacement.dll");
    fs::write(&staged, b"stable bytes").unwrap();
    let artifact = verified_artifact(
        PathBuf::from("Engine.dll"),
        Arc::from(&b"stable bytes"[..]),
        fs::metadata(&staged).unwrap().permissions(),
    );
    let manifest = serde_json::to_vec(&serde_json::json!({
        "files": [{
            "relative_path": "Engine.dll",
            "size": artifact.size(),
            "sha256": artifact.sha256_hex(),
        }]
    }))
    .unwrap();
    let package = VerifiedPackage {
        artifacts: vec![artifact],
        expected_names: BTreeSet::from(["engine.dll".to_owned()]),
    }
    .with_manifest_bytes(manifest.clone())
    .unwrap();
    fs::write(directory.path().join("build-manifest.json"), manifest).unwrap();
    let prepared = package
        .prepare_commit(directory.path(), "x86_64-pc-windows-msvc")
        .unwrap();

    fs::write(&replacement, b"stable bytes").unwrap();
    fs::remove_file(&staged).unwrap();
    symlink(&replacement, &staged).unwrap();
    assert!(prepared.verify_source_contents().is_err());
}

#[test]
fn prepared_package_rejects_replaced_staging_directory() {
    let parent = tempfile::tempdir().unwrap();
    let staging = parent.path().join("staging");
    fs::create_dir(&staging).unwrap();
    let staged = staging.join("Engine.dll");
    fs::write(&staged, b"stable bytes").unwrap();
    let artifact = verified_artifact(
        PathBuf::from("Engine.dll"),
        Arc::from(&b"stable bytes"[..]),
        fs::metadata(&staged).unwrap().permissions(),
    );
    let manifest = serde_json::to_vec(&serde_json::json!({
        "files": [{
            "relative_path": "Engine.dll",
            "size": artifact.size(),
            "sha256": artifact.sha256_hex(),
        }]
    }))
    .unwrap();
    let package = VerifiedPackage {
        artifacts: vec![artifact],
        expected_names: BTreeSet::from(["engine.dll".to_owned()]),
    }
    .with_manifest_bytes(manifest.clone())
    .unwrap();
    fs::write(staging.join("build-manifest.json"), manifest).unwrap();
    let prepared = package
        .prepare_commit(&staging, "x86_64-pc-windows-msvc")
        .unwrap();

    let moved = parent.path().join("moved-staging");
    fs::rename(&staging, &moved).unwrap();
    let replacement = PrivateStagingDirectory::create(&staging).unwrap();
    package.materialize(&replacement).unwrap();

    assert!(prepared.verify_source_contents().is_err());
}

#[test]
fn prepared_directory_rejects_nested_file_mutation() {
    let parent = tempfile::tempdir().unwrap();
    let staging = parent.path().join("staging");
    let package = staging.join("package-a");
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("manifest.json"), b"original manifest").unwrap();
    fs::write(package.join("addin.xll"), b"original addin").unwrap();

    let prepared = PreparedDirectoryCommit::prepare(&staging, &["package-a"]).unwrap();
    fs::write(package.join("addin.xll"), b"changed addin").unwrap();

    assert!(prepared.verify_source_contents().is_err());
}

#[cfg(unix)]
#[test]
fn private_staging_verify_rejects_replaced_path() {
    let parent = tempfile::tempdir().unwrap();
    let staging = parent.path().join("staging");
    let capability = PrivateStagingDirectory::create(&staging).unwrap();
    let moved = parent.path().join("moved-staging");
    fs::rename(&staging, &moved).unwrap();
    let replacement = PrivateStagingDirectory::create(&staging).unwrap();

    assert!(matches!(
        capability.verify(),
        Err(PackageError::StagingDirectoryReplaced { .. })
    ));

    drop(replacement);
    drop(capability);
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
    let staging_directory = PrivateStagingDirectory::create(&destination).unwrap();
    stage_bundle(&bundle, &staging_directory).unwrap();

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
    buf[opt + 0x70 + 13 * 8..opt + 0x70 + 13 * 8 + 4].copy_from_slice(&section_rva.to_le_bytes());
    buf[opt + 0x70 + 13 * 8 + 4..opt + 0x70 + 13 * 8 + 8].copy_from_slice(&0x40u32.to_le_bytes());

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
        executable_exports: BTreeSet::from(["xlAutoOpen".to_string(), "CustomEntry".to_string()]),
        expected_exports: BTreeSet::from(["xlAutoOpen".to_string()]),
        ..Default::default()
    };

    assert!(verify_xll_exports(&info, &xll_path, &[]).is_err());

    info.exports.remove("CustomEntry");
    info.executable_exports.remove("CustomEntry");
    info.nonzero_export_slots
        .extend([ExportAddressIndex(1), ExportAddressIndex(2)]);
    assert!(verify_xll_exports(&info, &xll_path, &[]).is_err());
}
