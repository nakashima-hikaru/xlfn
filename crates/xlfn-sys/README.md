# xlfn-sys

Raw Excel 12 XLL ABI definitions, `XLOPER12` types, and callback trampolines used by the `xlfn` runtime.

> **API Policy**: `xlfn` is the only supported public API for add-in authors. `xlfn-sys` is an implementation crate providing raw C-ABI bindings and does not guarantee standalone semantic versioning stability. Most applications should use [`xlfn`](https://crates.io/crates/xlfn) rather than calling these ABI definitions directly.

Documentation: [User guide](https://nakashima-hikaru.github.io/xlfn/) | [API docs](https://docs.rs/xlfn-sys) | [crates.io](https://crates.io/crates/xlfn-sys)
