use std::path::{Path, PathBuf};

const FILTERS: &[&str] = &[
    "tools/windows-bindings/filters/xlfn-sys.txt",
    "tools/windows-bindings/filters/cargo-xlfn.txt",
    "tools/windows-bindings/filters/xlfn-package.txt",
    "tools/windows-bindings/filters/xlfn-core.txt",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("generator must be located at tools/windows-bindings")
        .to_owned()
}

fn main() {
    let root = workspace_root();
    std::env::set_current_dir(&root).unwrap_or_else(|error| {
        panic!(
            "failed to switch to workspace root {}: {error}",
            root.display()
        )
    });

    for filter in FILTERS {
        windows_bindgen::bindgen(["--etc", filter]).unwrap();
    }
}
