# xlfn

The `xlfn` crate provides the framework for building native Rust Microsoft Excel XLL add-ins.

Most add-in projects should depend on this crate. It exposes typed worksheet functions, execution-aware contexts, value conversion, handles, native async UDFs, RTD subscriptions, and packaging integration through the `cargo-xlfn` CLI.

Place `#[excel_addin]` on the crate-root `impl Addin for MyAddin`. Add-ins without
RTD sources omit RTD associated types; the macro supplies the framework's empty
configuration when needed. Bare implementations and the former struct form
are unsupported: the macro follows unified Cargo features for the implementation.

Public API paths are grouped by responsibility: `addin` for lifecycle and contexts,
`task` for async execution, `rtd` and `handle` for capabilities, `value` for semantic
inputs, `value::raw` for safe XLOPER12 views, `output` for completed return buffers,
and `error` for application and framework failures. `prelude` covers common authoring.

Ordinary scalar inputs use strict conversion, including aliases. Opt into Excel
coercion using `#[excel_arg(native)]` or `#[excel_arg(native = f64)]` for aliases.
Applications can return `UdfResult<T>` to retain original causes before the Excel
boundary; diagnostics receive bounded snapshots of those causes.

`bench-internals`, `refinement`, and hidden exports are unsupported developer
interfaces and carry no semantic-versioning guarantees.

To learn more, read the [User Guide](https://nakashima-hikaru.github.io/xlfn/). See the [API docs](https://docs.rs/xlfn) or [crates.io](https://crates.io/crates/xlfn) for the published crate.
