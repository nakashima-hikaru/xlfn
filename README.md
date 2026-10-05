# xlfn

[![Crates.io](https://img.shields.io/crates/v/xlfn.svg)](https://crates.io/crates/xlfn)
[![docs.rs](https://docs.rs/xlfn/badge.svg)](https://docs.rs/xlfn)
[![CI](https://github.com/nakashima-hikaru/xlfn/actions/workflows/ci.yml/badge.svg)](https://github.com/nakashima-hikaru/xlfn/actions/workflows/ci.yml)
[![Rust 1.99.0](https://img.shields.io/badge/Rust-1.99.0-000000?logo=rust)](rust-toolchain.toml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Build native Microsoft Excel XLL add-ins in Rust. Write worksheet functions in Rust;
xlfn handles registration, value conversion, and the add-in lifecycle.

[User guide](https://nakashima-hikaru.github.io/xlfn/) ·
[API reference](https://docs.rs/xlfn) ·
[Examples](examples)

xlfn is pre-1.0. Public APIs may change between minor releases.

## Features

- Define Excel worksheet functions with ergonomic Rust macros.
- Build and package add-ins for 32-bit and 64-bit Excel with `cargo xlfn`.

Enable optional capabilities with Cargo features:

- `async` — Run asynchronous calculations and return results to Excel when ready.
- `rtd` — Stream live updates to worksheet cells through Excel's Real-Time Data API.
- `handles` — Keep Rust objects between calls and pass references to them through worksheet cells.
- `cache` — Cache calculation results for reuse across calls.
- `serde` — Serialize and deserialize owned worksheet values and collections.
- `chrono` / `time` — Convert serial dates using the corresponding calendar crate.

## Quick start

You need Windows 10 or 11, Rust **1.99.0 or later**, and Visual Studio Build Tools
with **Desktop development with C++**. Choose the target to match your Excel
installation: `x86_64-pc-windows-msvc` for 64-bit Excel or
`i686-pc-windows-msvc` for 32-bit Excel.

Create a project and install the tools for 64-bit Excel:

```powershell
cargo new --lib my-xll
cd my-xll
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xlfn --locked
```

Add these settings to `Cargo.toml`, using its existing `[dependencies]` section:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
xlfn = "0.2.0"
```

Replace `src/lib.rs` with:

```rust
use xlfn::prelude::*;

#[excel_addin(name = "Example Add-in", id = "example-addin", category = "Example")]
pub struct ExampleAddin;

impl Addin for ExampleAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> OpenResult<Self> {
        Ok(Opened::new(()))
    }
}

/// Adds two numbers.
#[excel_function(name = "EXAMPLE.ADD", thread_safe)]
pub fn add(left: f64, right: f64) -> f64 {
    left + right
}
```

Build the XLL:

```powershell
cargo xlfn package --target x86_64-pc-windows-msvc
```

In Excel, open **File → Options → Add-ins → Manage: Excel Add-ins → Go → Browse**
and select the `.xll` in `package/win-x64/`. Then enter:

```text
=EXAMPLE.ADD(2, 3)
```

For 32-bit Excel, use `i686-pc-windows-msvc` in both commands and load the XLL
from `package/win-x86/`.

## Learn more

See the [user guide](https://nakashima-hikaru.github.io/xlfn/) for configuration,
usage, and deployment.

Start with the [basic add-in](examples/basic-xll) example.

## Out of scope

- Ribbon UI
- Task panes
- General Office automation

## License

Licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
