# xlfn

[![CI](https://github.com/nakashima-hikaru/xlfn/actions/workflows/ci.yml/badge.svg)](https://github.com/nakashima-hikaru/xlfn/actions/workflows/ci.yml)
[![Rust 1.97.1](https://img.shields.io/badge/Rust-1.97.1-000000?logo=rust)](rust-toolchain.toml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

**xlfn** is a Rust framework for building native Microsoft Excel XLL add-ins against the Excel 12 / `XLOPER12` C API.

It turns typed Rust functions into Excel worksheet functions while handling registration, value conversion, return-value ownership, panic containment, lifecycle coordination, formula-owned handles, native asynchronous UDFs, RTD, and distribution validation.

> [!IMPORTANT]
> xlfn is currently a **pre-release `0.1.0` project**. It is not yet published to crates.io, its API may change, and production deployments should qualify the exact Windows and Excel versions they support.

## Why xlfn?

The raw XLL API is fast and flexible, but it exposes calling conventions, registration strings, tagged unions, manual memory ownership, COM-based RTD, and unload-time synchronization directly to application code.

xlfn keeps that machinery behind a typed Rust interface:

- ordinary worksheet functions are written as Rust functions;
- invalid execution-mode and argument combinations are rejected at compile time where possible;
- panics are contained before crossing the Excel ABI;
- typed handles have formula-aware ownership and replacement semantics;
- async UDFs, RTD, diagnostics, and shutdown share one lifecycle model;
- release packages are checked for architecture, exports, imports, and sidecar DLLs;
- both **32-bit Excel** and **64-bit Excel** are supported through the MSVC x86 and x64 targets.

xlfn deliberately does not provide a Ribbon, task-pane, IntelliSense, or general COM-automation framework.

## Quick start

### Requirements

- Windows 10 or Windows 11
- Rust `1.97.1`
- Visual Studio Build Tools with **Desktop development with C++**
- `i686-pc-windows-msvc` for 32-bit Excel and/or `x86_64-pc-windows-msvc` for 64-bit Excel

Choose the target from the **Excel process bitness**, not the Windows bitness. A 64-bit Windows installation can still run 32-bit Excel.

### Build the included example

```powershell
git clone https://github.com/nakashima-hikaru/xlfn.git
cd xlfn

cargo install --path crates/cargo-xlfn --locked --force

cargo xlfn dist `
    --manifest-path examples/basic-xll/Cargo.toml `
    --target x86_64-pc-windows-msvc `
    --locked
```

For 32-bit Excel, use `i686-pc-windows-msvc`. To build both architectures:

```powershell
cargo xlfn dist `
    --manifest-path examples/basic-xll/Cargo.toml `
    --all `
    --locked
```

Load the generated `.xll` from the matching `dist/win-x64` or `dist/win-x86` directory through:

**File → Options → Add-ins → Manage: Excel Add-ins → Go → Browse**

See the [quick-start guide](guide/src/quick-start.md) for creating a project, packaging native DLLs, and troubleshooting.
xlfn

## Programming model

Define one add-in and annotate exported worksheet functions:

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

The macro generates the Excel-visible exports and registration metadata. Inputs are decoded through typed conversion traits, and ordinary Rust return types or `Result<T, E>` values are converted back to Excel values.

Additional APIs cover:

- [execution contexts](guide/src/execution-modes.md);
- [typed formula-owned handles](guide/src/handles.md);
- [native asynchronous functions](guide/src/async-functions.md);
- [streaming RTD](guide/src/rtd.md);
- [custom value conversions](guide/src/custom-conversions.md);
- [native DLL integration](guide/src/native-overview.md).

## How does it compare?

| Choose                   | Best fit                                                                                                                  | Excel bitness                                         |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------- |
| **xlfn**                 | Native Rust add-ins that need typed XLL boundaries, explicit ownership, async/RTD support, and validated native packaging | 32-bit and 64-bit                                     |
| **Excel-DNA**            | .NET add-ins, especially those using the broader .NET, Office UI, IntelliSense, or COM ecosystem                          | 32-bit and 64-bit in the standard runtime-based model |
| **Excel-DNA Native AOT** | Self-contained .NET Native AOT add-ins without a separately installed .NET runtime                                        | Currently 64-bit only                                 |
| **Direct C/C++ XLL SDK** | Teams that require complete low-level control and are prepared to own the ABI, conversion, lifetime, and packaging layers | 32-bit and 64-bit                                     |

This is a scope comparison, not a performance benchmark. Excel-DNA is an established project with a substantially broader ecosystem and production history. xlfn is intended for projects where Rust and native lifecycle control are primary requirements.

References: [Excel-DNA](https://excel-dna.net/docs/introduction/), [Excel-DNA Native AOT](https://excel-dna.github.io/docs/guides-basic/dotnet-native-aot-support/), and [Microsoft XLL development documentation](https://learn.microsoft.com/en-us/office/client-developer/excel/developing-excel-xlls).
xlfn

## Lean 4 formalization

The [`formal/`](formal) directory contains an executable Lean 4 transition-system model of the shutdown protocol. It proves properties of the abstract lifecycle model, including that successful shutdown reaches a quiescent state and that closed or fail-stopped states are terminal.

The scope is intentionally limited: this is **not** yet a machine-checked proof that every Rust implementation path refines the Lean model, nor a proof of the entire XLL implementation. The [formal model README](formal/README.md) lists the proved theorems, assumptions, and remaining refinement work.

CI builds the Lean project, runs `leanchecker`, and rejects committed `sorry` or `admit` placeholders.

## Documentation

- [User guide](https://nakashima-hikaru.github.io/xlfn/)
- [Quick start](guide/src/quick-start.md)
- [Compatibility](guide/src/compatibility.md)
- [CLI reference](guide/src/cli-reference.md)
- [Testing and release qualification](guide/src/testing.md)
- [Security model](guide/src/security.md)
- [Basic example](examples/basic-xll)

## Project status

Implemented areas include typed registration and conversion, lifecycle management, handles, async UDFs, RTD, diagnostics, caches, thread-bound worker support, package staging, PE validation, and the Lean shutdown model.

The public API is not stable at `0.1.0`. Automated tests and artifact inspection also do not replace qualification in the exact Excel versions, channels, locales, and deployment environment used in production.

## Contributing

Contributions should keep unsafe Excel ABI operations inside narrow, documented boundaries and prefer compile-time validation over runtime interpretation. Changes to unsafe code should include explicit `SAFETY` reasoning and focused tests; changes to shutdown semantics should update the Lean model or its refinement obligations where applicable.

Before opening a pull request, run:

```console
cargo fmt --all -- --check
cargo clippy --workspace --exclude excel-abi-probe --all-targets --all-features --locked -- -D warnings
cargo test --workspace --exclude excel-abi-probe --all-targets --all-features --locked
python3 guide/check.py
```

## Security

See [SECURITY.md](SECURITY.md) for supported versions and private vulnerability reporting. The detailed runtime and deployment trust model is documented in the [security guide](guide/src/security.md).

XLLs and bundled DLLs are executable code. Control and sign release artifacts as appropriate, and do not treat build-manifest hashes alone as a runtime trust boundary.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)); or
- MIT License ([LICENSE-MIT](LICENSE-MIT)).

at your option.
