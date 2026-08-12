# xlfn

[![Crates.io](https://img.shields.io/crates/v/xlfn.svg)](https://crates.io/crates/xlfn)
[![docs.rs](https://docs.rs/xlfn/badge.svg)](https://docs.rs/xlfn)
[![CI](https://github.com/nakashima-hikaru/xlfn/actions/workflows/ci.yml/badge.svg)](https://github.com/nakashima-hikaru/xlfn/actions/workflows/ci.yml)
[![Rust 1.97.1](https://img.shields.io/badge/Rust-1.97.1-000000?logo=rust)](rust-toolchain.toml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

**xlfn** is a Rust framework for building native Microsoft Excel XLL add-ins.

Define worksheet functions as typed Rust functions while xlfn handles the Excel 12 / `XLOPER12` boundary, registration, value conversion, runtime lifecycle, and packaging.

xlfn supports both 32-bit and 64-bit Excel and does **not** require a .NET runtime.

**[User Guide](https://nakashima-hikaru.github.io/xlfn/)**

> [!NOTE]
> xlfn is pre-1.0. Public APIs may change between minor releases.

## Features

- **Typed worksheet functions** — define Excel functions as ordinary Rust functions with generated registration, typed argument and result conversion, metadata, and signature validation.

- **Execution-aware APIs** — express main-thread, thread-safe, macro-sheet, and asynchronous execution requirements through explicit capabilities and contexts.

- **Stateful and long-running calculation** — use formula-owned typed handles, native asynchronous UDFs, streaming RTD subscriptions, and calculation caches.

- **Runtime safety** — contain panics at the Excel boundary and coordinate return-value ownership, add-in lifecycle, shutdown, diagnostics, and concurrent activity.

- **Native deployment** — build native XLLs for both 32-bit and 64-bit Excel, with transactional packaging and validation of PE architecture, exports, imports, and optional sidecar DLLs.

xlfn focuses on native worksheet-function and calculation infrastructure. It does not provide Ribbon UI, task panes, IntelliSense, or a general Office automation framework.

## Quick start

### Requirements

- Windows 10 or Windows 11
- Rust `1.97.1`
- Visual Studio Build Tools with **Desktop development with C++**
- `i686-pc-windows-msvc` for 32-bit Excel
- `x86_64-pc-windows-msvc` for 64-bit Excel

Choose the Rust target based on the **Excel process bitness**, not the Windows bitness. A 64-bit Windows installation may run either 32-bit or 64-bit Excel.

### Create an add-in

Add the Rust targets for the Excel architectures you want to support:

```powershell
rustup target add i686-pc-windows-msvc x86_64-pc-windows-msvc
```

Create a library crate:

```powershell
cargo new --lib my-xll
cd my-xll
```

Configure it as a `cdylib` and add xlfn:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
xlfn = "0.2.0"
```

Install `cargo-xlfn` for XLL packaging and artifact validation:

```powershell
cargo install cargo-xlfn --locked
```

Package for 64-bit Excel:

```powershell
cargo xlfn package --target x86_64-pc-windows-msvc
```

For 32-bit Excel:

```powershell
cargo xlfn package --target i686-pc-windows-msvc
```

Or package both architectures:

```powershell
cargo xlfn package --all
```

The packaged XLLs are written under:

```text
package/
├── win-x64/
└── win-x86/
```

Load the appropriate `.xll` in Excel using:

**File → Options → Add-ins → Manage: Excel Add-ins → Go → Browse**

See the [User Guide](https://nakashima-hikaru.github.io/xlfn/) for project setup, deployment, compatibility, and troubleshooting.

## Example

Define an add-in and export a worksheet function:

```rust
#![deny(unsafe_code)]

use xlfn::prelude::*;

pub struct State;

#[excel_addin(
    name = "Example Add-in",
    id = "example-addin",
    category = "Example"
)]
pub struct ExampleAddin;

impl Addin for ExampleAddin {
    type State = State;
    type Error = XllError;

    fn open(_context: &OpenContext) -> Result<State, XllError> {
        Ok(State)
    }
}

/// Adds two finite numbers.
#[excel_function(name = "EXAMPLE.ADD", thread_safe)]
pub fn add(
    #[excel_arg(description = "First addend.")] left: f64,
    #[excel_arg(description = "Second addend.")] right: f64,
) -> f64 {
    left + right
}
```

The generated XLL exports the required Excel entry points and registers the function as `EXAMPLE.ADD`.

Arguments are decoded through typed conversion traits, and Rust return values are converted back into Excel-compatible results.

## Comparison

|                                 | **xlfn**                        | **Excel-DNA**                   | **Direct XLL SDK**        |
| ------------------------------- | ------------------------------- | ------------------------------- | ------------------------- |
| Primary ecosystem               | Rust                            | .NET                            | C / C++                   |
| Worksheet functions             | Typed Rust API                  | Managed framework API           | Raw Excel C API           |
| Excel ABI handling              | Framework-managed               | Framework-managed               | Application-managed       |
| Handles / long-lived objects    | Typed formula-owned handles     | Framework facilities            | Application-defined       |
| Async / streaming calculation   | Native async UDFs and RTD       | Supported                       | Application-defined       |
| UI / broader Office integration | Not a goal                      | Broad                           | Manual / COM              |
| Packaging validation            | Integrated with `cargo xlfn`    | Framework tooling               | Application-defined       |
| Best fit                        | Native Rust calculation add-ins | .NET and broader Office add-ins | Maximum low-level control |

xlfn deliberately focuses on a small, native, type-safe foundation for worksheet functions and calculation engines rather than the broader Excel UI and Office automation surface.

## Formal verification

Parts of xlfn's lifecycle and shutdown protocols are formally modeled in Lean 4.

The [`formal/`](formal) project contains:

- a model of the resource shutdown protocol;
- a separate model of lifecycle synchronization around open and final close;
- a composition model connecting lifecycle and shutdown behavior;
- safety and invariant proofs;
- executable trace checkers; and
- refinement obligations connecting concrete runtime behavior to the abstract models.

CI builds the Lean project and checks generated runtime traces against the executable models.

The formalization proves properties of the abstract models and their stated refinement boundaries. It is **not** a proof of the complete Rust implementation or of arbitrary application code running inside an add-in.

See [`formal/README.md`](formal/README.md) for the models, proved properties, assumptions, and remaining refinement obligations.

## Security

See [SECURITY.md](SECURITY.md) for supported versions and vulnerability reporting.

XLLs and bundled DLLs are executable code. Release artifacts should be controlled and code-signed as appropriate.

The detailed runtime and deployment trust model is documented in the [User Guide](https://nakashima-hikaru.github.io/xlfn/security.html).

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)); or
- MIT License ([LICENSE-MIT](LICENSE-MIT)).

at your option.
