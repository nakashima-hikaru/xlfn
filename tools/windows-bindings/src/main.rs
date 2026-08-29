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
        "tools/windows-bindings/filters/cargo-xlfn.txt",
        "crates/cargo-xlfn/src/win32.rs",
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

const ALLOW_SUFFIX_LF: &str = " clippy::all\n)]";
const ALLOW_SUFFIX_CRLF: &str = " clippy::all\r\n)]";
const ALLOW_SUFFIX_WITH_REASON_LF: &str =
    " unreachable_pub,\n    clippy::all,\n    reason = \"Generated code from windows-bindgen\"\n)]";
const ALLOW_SUFFIX_WITH_REASON_CRLF: &str = " unreachable_pub,\r\n    clippy::all,\r\n    reason = \"Generated code from windows-bindgen\"\r\n)]";
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

fn add_generated_allow_reason(path: &Path) {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let lf_count = source.matches(ALLOW_SUFFIX_LF).count();
    let crlf_count = source.matches(ALLOW_SUFFIX_CRLF).count();
    let (needle, replacement) = match (lf_count, crlf_count) {
        (1, 0) => (ALLOW_SUFFIX_LF, ALLOW_SUFFIX_WITH_REASON_LF),
        (0, 1) => (ALLOW_SUFFIX_CRLF, ALLOW_SUFFIX_WITH_REASON_CRLF),
        _ => panic!(
            "expected exactly one generated clippy::all suffix in {}; found LF={lf_count}, CRLF={crlf_count}",
            path.display()
        ),
    };
    let patched = source.replacen(needle, replacement, 1);
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
        windows_bindgen::bindgen(["--etc", filter]).unwrap();
        let output = Path::new(output);
        add_generated_allow_reason(output);
        remove_unused_tlibattr_default(output);
    }
}
