# xlfn

[![Crates.io](https://img.shields.io/crates/v/xlfn.svg)](https://crates.io/crates/xlfn)
[![docs.rs](https://docs.rs/xlfn/badge.svg)](https://docs.rs/xlfn)
[![CI](https://github.com/nakashima-hikaru/xlfn/actions/workflows/ci.yml/badge.svg)](https://github.com/nakashima-hikaru/xlfn/actions/workflows/ci.yml)
[![Rust 1.97.1](https://img.shields.io/badge/Rust-1.97.1-000000?logo=rust)](rust-toolchain.toml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

**xlfn** is a Rust framework for building native Microsoft Excel XLL add-ins.

Write worksheet functions as typed Rust functions and let xlfn handle the Excel 12 / `XLOPER12` boundary, including registration, value conversion, return-value ownership, panic containment, handles, asynchronous UDFs, RTD, shutdown, and packaging.

xlfn produces native XLLs and does **not** require the .NET runtime.

> [!IMPORTANT]
> xlfn is currently at version `0.1.0`. The public API may change, and production deployments should be tested against the exact Windows and Excel versions they support.

## Why xlfn?

The Excel XLL API offers low overhead and direct access to native code, but using it safely requires substantial infrastructure around calling conventions, tagged unions, memory ownership, registration, COM-based RTD, and DLL shutdown.

xlfn provides that infrastructure behind a typed Rust API:

- write worksheet functions as ordinary Rust functions;
- use native binaries without a .NET runtime dependency;
- catch panics before they cross the Excel ABI;
- reject invalid function signatures and execution modes where possible;
- return typed, formula-owned handles;
- build native asynchronous UDFs and streaming RTD functions;
- support both 32-bit and 64-bit Excel;
- validate packaged XLLs, exports, imports, architectures, and sidecar DLLs.

xlfn focuses on native worksheet-function infrastructure. It does not provide a Ribbon, task panes, IntelliSense, or a general Office automation framework.

## Quick start

### Requirements

- Windows 10 or Windows 11
- Rust `1.97.1`
- Visual Studio Build Tools with **Desktop development with C++**
- `i686-pc-windows-msvc` for 32-bit Excel
- `x86_64-pc-windows-msvc` for 64-bit Excel

Choose the target based on the **Excel process bitness**, not the Windows bitness. A 64-bit Windows installation may run either 32-bit or 64-bit Excel.

### Create an add-in

Install the CLI and Rust targets:

```powershell
cargo install cargo-xlfn --locked
rustup target add i686-pc-windows-msvc x86_64-pc-windows-msvc
```

Create a project:

```powershell
cargo xlfn new my-xll
cd my-xll
```

Build a distribution for 64-bit Excel:

```powershell
cargo xlfn dist --target x86_64-pc-windows-msvc
```

For 32-bit Excel:

```powershell
cargo xlfn dist --target i686-pc-windows-msvc
```

To build both:

```powershell
cargo xlfn dist --all
```

Load the generated `.xll` from `dist/win-x64` or `dist/win-x86` using:

**File → Options → Add-ins → Manage: Excel Add-ins → Go → Browse**

See the [quick-start guide](guide/src/quick-start.md) for sidecar packaging, deployment, and troubleshooting.

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

Inputs are decoded through typed conversion traits. Rust values and `Result<T, E>` values are converted back into Excel-compatible results.

## Features

- [Execution modes and contexts](guide/src/execution-modes.md)
- [Typed formula-owned handles](guide/src/handles.md)
- [Native asynchronous functions](guide/src/async-functions.md)
- [Streaming RTD](guide/src/rtd.md)
- [Custom value conversions](guide/src/custom-conversions.md)
- [Add-in lifecycle and state](guide/src/lifecycle.md)
- Diagnostics and panic containment
- Package staging and PE validation
- Static or dynamic MSVC CRT policies

## Comparison

| Approach                 | Best suited for                                                                                                                         |
| ------------------------ | --------------------------------------------------------------------------------------------------------------------------------------- |
| **xlfn**                 | Native Rust XLLs with typed boundaries, handles, async UDFs, RTD, and validated packaging                                               |
| **Excel-DNA**            | .NET-based add-ins that need access to the broader .NET and Office ecosystem                                                            |
| **Direct C/C++ XLL SDK** | Teams that want complete low-level control and are prepared to implement the ABI, ownership, lifecycle, and packaging layers themselves |

xlfn is not intended to replace the broader UI and automation ecosystems around Excel. Its focus is a small, native, type-safe foundation for worksheet functions and native calculation engines.

## Formal shutdown model

The [`formal/`](formal) directory contains an executable Lean 4 model of the shutdown protocol.

The model proves properties of the abstract lifecycle state machine, including that successful shutdown reaches a quiescent state and that terminal states cannot reopen unexpectedly.

This is not yet a proof of the entire Rust implementation. The [formal model README](formal/README.md) documents the proved theorems, assumptions, and remaining refinement work.

CI builds the Lean project, runs `leanchecker`, and rejects committed `sorry` or `admit` placeholders.

## Documentation

- [User guide](https://nakashima-hikaru.github.io/xlfn/)
- [Quick start](guide/src/quick-start.md)
- [Compatibility](guide/src/compatibility.md)
- [CLI reference](guide/src/cli-reference.md)
- [Testing and release qualification](guide/src/testing.md)
- [Security model](guide/src/security.md)
- [Basic example](examples/basic-xll)

## Status

xlfn currently includes:

- typed function registration and value conversion;
- lifecycle and unload coordination;
- formula-owned handles;
- asynchronous UDFs;
- RTD;
- diagnostics and caches;
- distribution staging and native artifact validation;
- a Lean 4 shutdown model.

Application-specific domain logic, native-library bindings, services, worker processes, and downstream protocols remain the responsibility of the add-in.

Because the project is still at `0.1.0`, API stability is not guaranteed. Automated tests and artifact validation do not replace testing in the exact Excel channels, locales, architectures, and deployment environments used in production.

## Security

See [SECURITY.md](SECURITY.md) for supported versions and vulnerability reporting.

The detailed runtime and deployment trust model is described in the [security guide](guide/src/security.md).

XLLs and bundled DLLs are executable code. Release artifacts should be controlled and code-signed as appropriate. Build-manifest hashes alone are not a runtime trust boundary.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)); or
- MIT License ([LICENSE-MIT](LICENSE-MIT)).

at your option.
