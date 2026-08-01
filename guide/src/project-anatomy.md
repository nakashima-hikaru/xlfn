# Project anatomy

A production add-in is normally one Rust crate. Separate crates are optional architectural choices, not framework requirements.

```text
hello-xlfn/
├── Cargo.toml
├── src/
│   ├── lib.rs       # one add-in definition and lifecycle state
│   └── udf.rs       # exported worksheet functions
└── native/          # optional packaged DLLs by bitness
    ├── x86/
    └── x64/
```

## `src/lib.rs`: lifecycle and shared state

Place exactly one `#[excel_addin]` type at crate root. Its `Addin` implementation creates immutable or internally synchronized state that exported functions access through typed contexts.

```rust
use xlfn::prelude::*;
use std::sync::Arc;

pub struct State {
    pub configuration: Arc<Configuration>,
}

pub struct Configuration {
    pub desk: String,
}

#[excel_addin(name = "Desk Tools", id = "desk-tools", category = "Desk")]
pub struct DeskTools;

impl Addin for DeskTools {
    type State = State;
    type Error = XllError;

    fn open(context: &OpenContext) -> XllResult<State> {
        let configuration = load_configuration(context.module_directory())?;
        Ok(State {
            configuration: Arc::new(configuration),
        })
    }
}

fn load_configuration(_: &std::path::Path) -> XllResult<Configuration> {
    Ok(Configuration {
        desk: "Rates".to_owned(),
    })
}
```

`State` must be `Send + Sync + 'static`. Thread-affine owners therefore do not belong directly in state; keep those owners in a lifecycle scope and expose only safe, cloneable handles. The native chapters cover this pattern.

## UDF modules

Worksheet functions may be organized across ordinary Rust modules. The macro uses inventory-based registration, so there is no central handwritten function table.

```rust
mod data;
mod math;
mod text;
```

Each Excel-visible function remains a non-generic, safe, free Rust function. Internal helpers may use any appropriate Rust design.

## Cargo metadata

Cargo metadata controls output names and native-file placement. It is not a second source of truth for worksheet signatures or native ABIs.

- `#[excel_function]` is the source of truth for worksheet metadata.
- `#[excel_addin]` is the source of truth for add-in identity and lifecycle exports.
- Native FFI binding declarations are the source of truth for native function signatures.
- `[package.metadata.xlfn]` controls distribution artifact naming and bundle staging.

## Generated boundary

At compile time, the macros generate:

- lifecycle and COM exports;
- one ABI wrapper and one registration descriptor per function;
- x86 decorated export directives where required;
- a `.xllexp` manifest section for linked-artifact validation;
- conversion, panic, and return-ownership boundaries;
- optional asynchronous calculation-event exports.

Application code should not call the generated symbols directly. Test application logic through ordinary Rust functions and integration behavior through `cargo xlfn check` and real Excel.
