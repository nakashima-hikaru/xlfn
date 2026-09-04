use std::{
    fs,
    path::{Path, PathBuf},
};

const BINDINGS: &[(&str, &str)] = &[
    (
        "tools/windows-bindings/filters/xlfn-sys.txt",
        "crates/xlfn-sys/src/win32.rs",
    ),
    (
        "tools/windows-bindings/filters/xlfn-package.txt",
        "crates/xlfn-package/src/win32.rs",
    ),
    (
        "tools/windows-bindings/filters/xlfn.txt",
        "crates/xlfn/src/win32.rs",
    ),
];

const ALLOW_HEADER: &str = r#"#![allow(
    non_snake_case,
    non_upper_case_globals,
    non_camel_case_types,
    dead_code,
    unreachable_pub,
    clippy::all,
    reason = "Generated code from windows-bindgen"
)]

"#;
const TLIBATTR_DEFAULT_DERIVE_LF: &str = "#[derive(Clone, Copy, Default)]\npub struct TLIBATTR";
const TLIBATTR_DEFAULT_DERIVE_CRLF: &str = "#[derive(Clone, Copy, Default)]\r\npub struct TLIBATTR";
const TLIBATTR_NO_DEFAULT_DERIVE_LF: &str = "#[derive(Clone, Copy)]\npub struct TLIBATTR";
const TLIBATTR_NO_DEFAULT_DERIVE_CRLF: &str = "#[derive(Clone, Copy)]\r\npub struct TLIBATTR";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("generator must be located at tools/windows-bindings")
        .to_owned()
}

fn prepend_generated_allow_header(path: &Path) {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let patched = format!("{ALLOW_HEADER}{source}");
    fs::write(path, patched)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", path.display()));
}

fn remove_unused_tlibattr_default(path: &Path) {
    // `TLIBATTR` is only used through ABI pointers in xlfn. The generated
    // `Default` implementation is therefore unused, and it becomes invalid
    // when the intentionally non-Default `GUID` field is generated alongside
    // it.
    let source = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let (needle, replacement) = if source.contains(TLIBATTR_DEFAULT_DERIVE_LF) {
        (TLIBATTR_DEFAULT_DERIVE_LF, TLIBATTR_NO_DEFAULT_DERIVE_LF)
    } else if source.contains(TLIBATTR_DEFAULT_DERIVE_CRLF) {
        (
            TLIBATTR_DEFAULT_DERIVE_CRLF,
            TLIBATTR_NO_DEFAULT_DERIVE_CRLF,
        )
    } else {
        return;
    };
    let patched = source.replacen(needle, replacement, 1);
    fs::write(path, patched)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", path.display()));
}

fn main() {
    let root = workspace_root();
    std::env::set_current_dir(&root).unwrap_or_else(|error| {
        panic!(
            "failed to switch to workspace root {}: {error}",
            root.display()
        )
    });

    for &(filter, output) in BINDINGS {
        windows_bindgen::bindgen(["--etc", filter]);
        let output = Path::new(output);
        prepend_generated_allow_header(output);
        remove_unused_tlibattr_default(output);
    }
}
