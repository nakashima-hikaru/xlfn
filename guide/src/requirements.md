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
- Rust **1.99.0 or later**;
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
| `async`   | Asynchronous functions returning results when completed |
| `handles` | Rust objects owned by worksheet formulas                |
| `rtd`     | Streaming Real-Time Data updates to cells               |
| `cache`   | Calculation caching across worksheet calls              |

For example:

```toml
[dependencies]
xlfn = { version = "0.2", features = ["async"] }
```

Continue with [Create your first add-in](quick-start.md).
