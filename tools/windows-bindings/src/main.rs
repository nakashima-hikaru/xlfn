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
    }
}
