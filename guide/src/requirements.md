# Requirements and setup

Prepare a Windows build environment and install the target matching your
Excel installation.

## Choose the Excel bitness

In Excel, open **File → Account → About Excel** to check whether your Excel
process is 32-bit or 64-bit. Match that process bitness, even if Windows itself is 64-bit.

| Excel process | Rust target              | Package directory  |
| ------------- | ------------------------ | ------------------ |
| 64-bit Excel  | `x86_64-pc-windows-msvc` | `package/win-x64/` |
| 32-bit Excel  | `i686-pc-windows-msvc`   | `package/win-x86/` |

## Prerequisites

You need:

- Windows 10 or Windows 11 with desktop Excel;
- Rust **1.100.0 or later**;
- Visual Studio Build Tools with the **Desktop development with C++** workload.

Add the target corresponding to your Excel installation:

```powershell
# For 64-bit Excel
rustup target add x86_64-pc-windows-msvc

# For 32-bit Excel
rustup target add i686-pc-windows-msvc
```

If Cargo cannot find the MSVC linker, run your build from the Developer PowerShell
supplied by Visual Studio Build Tools.

## Rust version support

The minimum supported Rust version (MSRV) is declared by
`workspace.package.rust-version` in the repository's `Cargo.toml`. The repository
pins that stable toolchain in `rust-toolchain.toml` for normal builds and CI.
Verification tools can use separately pinned toolchains.

During pre-1.0 development, releases may raise the MSRV to use required
standard-library APIs or dependency versions. Such changes are recorded in the
release notes. Check the new release's compiler requirement before upgrading an
add-in in an environment with an approved toolchain list.

## Install `cargo-xlfn`

Install the packaging and validation CLI:

```powershell
cargo install cargo-xlfn --locked
```

Alternatively, if building from a local repository checkout:

```powershell
cargo install --path crates/cargo-xlfn --locked
```

## Optional capabilities

The basic add-in requires no Cargo features. Enable optional features in your
`Cargo.toml` when needed:

| Feature   | Description                                             |
| --------- | ------------------------------------------------------- |
| `async` | Native async transport, task lifecycle, and external executor API |
| `async-builtin` | Async functions using the built-in polling executor |
| `handles` | Rust objects owned by worksheet formulas                |
| `rtd`     | Streaming Real-Time Data updates to cells               |
| `cache`   | Calculation caching across worksheet calls              |
| `serde`   | Serialization of owned worksheet values and collections |
| `jiff`    | Serial-date conversion with `jiff::civil::Date`           |

The `serde` feature controls serialization implementations for worksheet values.
The framework also uses `serde` and `serde_json` for structured diagnostic logs,
so those dependencies are present with or without the feature.

For example:

```toml
[dependencies]
xlfn = { version = "0.3", features = ["async-builtin"] }
```

Continue with [Create your first add-in](quick-start.md).
