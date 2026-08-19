# xlfn

[![Crates.io](https://img.shields.io/crates/v/xlfn.svg)](https://crates.io/crates/xlfn)
[![docs.rs](https://docs.rs/xlfn/badge.svg)](https://docs.rs/xlfn)
[![CI](https://github.com/nakashima-hikaru/xlfn/actions/workflows/ci.yml/badge.svg)](https://github.com/nakashima-hikaru/xlfn/actions/workflows/ci.yml)
[![Rust 1.97.1](https://img.shields.io/badge/Rust-1.97.1-000000?logo=rust)](rust-toolchain.toml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

[User Guide](https://nakashima-hikaru.github.io/xlfn/) ·
[API Docs](https://docs.rs/xlfn) ·
[crates.io](https://crates.io/crates/xlfn)

**xlfn is a Rust framework for building native Microsoft Excel XLL add-ins.**

Write worksheet functions as typed Rust functions. xlfn handles the Excel 12 / `XLOPER12` ABI, function registration, value conversion, runtime lifecycle, and XLL packaging.

- Native 32-bit and 64-bit Excel support
- No .NET runtime
- Typed synchronous, asynchronous, and streaming APIs
- Typed handles and calculation state
- Integrated XLL packaging and validation

> [!NOTE]
> xlfn is pre-1.0. Public APIs may change between minor releases.

## Example

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
    type Layers = ();

    fn open(_context: &OpenContext) -> Result<State, XllError> {
        Ok(State)
    }

    fn udf_layers(_state: &State) -> Self::Layers {}
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

This registers a thread-safe Excel worksheet function named `EXAMPLE.ADD`.

xlfn generates the required Excel entry points, validates the function signature, converts Excel arguments into Rust values, and converts the result back to Excel.

## Features

### Typed worksheet functions

Define Excel functions as ordinary typed Rust functions. xlfn generates registration metadata, argument and result conversion, and signature validation automatically.

### Execution-aware APIs

Explicit APIs represent Excel execution constraints, including:

- main-thread execution;
- thread-safe worksheet functions;
- macro-sheet access; and
- native asynchronous calculation.

### Stateful calculation

xlfn provides infrastructure for calculations that extend beyond a single function call:

- formula-owned typed handles;
- native asynchronous UDFs;
- RTD streaming subscriptions; and
- calculation caches.

### Runtime safety

The runtime manages the Excel boundary, including panic containment, return-value ownership, concurrent activity, diagnostics, lifecycle transitions, and shutdown.

### Native deployment

`cargo xlfn` builds and validates distributable XLL packages, including:

- 32-bit and 64-bit targets;
- PE architecture and export validation;
- import validation;
- optional sidecar DLLs; and
- transactional packaging.

xlfn focuses on native worksheet functions and calculation infrastructure. Ribbon UI, task panes, IntelliSense, and general Office automation are outside its scope.

## Getting started

### Requirements

- Windows 10 or Windows 11
- Rust `1.97.1`
- Visual Studio Build Tools with **Desktop development with C++**
- `i686-pc-windows-msvc` for 32-bit Excel
- `x86_64-pc-windows-msvc` for 64-bit Excel

The target architecture must match the **Excel process**, not the Windows installation. For example, 32-bit Excel on 64-bit Windows requires `i686-pc-windows-msvc`.

### Create a project

Install the Rust targets you need:

```powershell
rustup target add i686-pc-windows-msvc x86_64-pc-windows-msvc
```

Create a library crate:

```powershell
cargo new --lib my-xll
cd my-xll
```

Configure it as a `cdylib`:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
xlfn = "0.2.0"
```

Install the packaging tool:

```powershell
cargo install cargo-xlfn --locked
```

### Package the XLL

For 64-bit Excel:

```powershell
cargo xlfn package --target x86_64-pc-windows-msvc
```

For 32-bit Excel:

```powershell
cargo xlfn package --target i686-pc-windows-msvc
```

Or package both:

```powershell
cargo xlfn package --all
```

Artifacts are written to:

```text
package/
├── win-x64/
└── win-x86/
```

Load the appropriate `.xll` through:

**File → Options → Add-ins → Manage: Excel Add-ins → Go → Browse**

See the [User guide](https://nakashima-hikaru.github.io/xlfn/) for project setup, deployment, compatibility, and troubleshooting.

## Choosing an approach

| | **xlfn** | **Excel-DNA** | **Excel XLL SDK** |
|---|---|---|---|
| Language | Rust | .NET | C / C++ |
| Excel ABI | Managed by xlfn | Managed by Excel-DNA | Direct |
| Typed worksheet API | Rust | .NET | Application-defined |
| Handles | Built in | Built in | Application-defined |
| Async / RTD | Built in | Built in | Application-defined |
| XLL packaging | `cargo xlfn` | Excel-DNA tooling | Application-defined |
| Office UI integration | Out of scope | Extensive | Manual / COM |
| Best suited for | Native Rust calculation add-ins | .NET / Office integration | Low-level Excel integration |

Use **xlfn** when the calculation engine and worksheet API should remain native Rust.

Use **Excel-DNA** when broader .NET and Office integration is important.

Use the **Excel XLL SDK directly** when complete control over the Excel C API is more important than framework-level abstractions.

## Formal verification

xlfn uses Lean 4 to model and verify parts of its lifecycle and shutdown protocols.

The [`formal/`](formal) project covers lifecycle synchronization, resource shutdown, their composition, safety invariants, executable trace checking, and refinement obligations between the runtime and abstract models.

CI builds the formal models and checks generated runtime traces against them.

The proofs apply to the stated abstract models and refinement boundaries; they are not a proof of the complete Rust implementation or arbitrary add-in code.

See [`formal/README.md`](formal/README.md) for details.

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)); or
- MIT License ([LICENSE-MIT](LICENSE-MIT)).

at your option.
