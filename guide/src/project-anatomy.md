# Project anatomy

A production add-in is normally one Rust crate. Separate crates are optional architectural choices, not framework requirements.

```text
hello-xlfn/
├── Cargo.toml
└── src/
    ├── lib.rs       # one add-in definition and lifecycle state
    └── udf.rs       # exported worksheet functions
```

This is the framework-level shape, not a required application architecture. Add any modules, workspace crates, generated bindings, data directories, or sidecar directories that the application itself needs. xlfn only gives such files special meaning when they are referenced by an xlfn build or packaging contract.

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
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(context: &OpenContext) -> XllResult<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>> {
        let configuration = load_configuration(context.module_directory())?;
        Ok(Opened::new(State {
            configuration: Arc::new(configuration),
        }, (), ()))
    }
}

fn load_configuration(_: &std::path::Path) -> XllResult<Configuration> {
    Ok(Configuration {
        desk: "Rates".to_owned(),
    })
}
```

`State` must be `Send + Sync + 'static`. Anything stored in it must independently satisfy the concurrency contract of every worksheet function that can access it. If an application resource is thread-affine, expose only a safe thread-compatible client through `State` and retain creation, destruction, and join ownership in application lifecycle code. See [Add-in lifecycle and state](lifecycle.md) and [Execution modes and contexts](execution-modes.md).

## UDF modules

Worksheet functions may be organized across ordinary Rust modules. The macro uses inventory-based registration, so there is no central handwritten function table.

```rust
mod data;
mod math;
mod text;
```

Each Excel-visible function remains a non-generic, safe, free Rust function. Internal helpers may use any appropriate Rust design.

## Cargo metadata

Cargo metadata controls output names and optional sidecar-file placement. It is not a second source of truth for worksheet signatures or application runtime behavior.

- `#[excel_function]` is the source of truth for worksheet metadata.
- `#[excel_addin]` is the source of truth for add-in identity and lifecycle exports.
- Ordinary application code is the source of truth for domain behavior and downstream dependency contracts.
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
