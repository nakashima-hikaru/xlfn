# Requirements and setup

Prepare a Windows build environment and choose the target that matches your
Excel installation. These are the prerequisites for
[Create your first add-in](quick-start.md); no xlfn knowledge is required.

## Choose the Excel bitness

In Excel, open **File → Account → About Excel** to find whether the Excel
process is 32-bit or 64-bit. Match that process, even if Windows itself is 64-bit.

| Excel process | Rust target | Package directory |
| --- | --- | --- |
| 64-bit Excel | `x86_64-pc-windows-msvc` | `package/win-x64/` |
| 32-bit Excel | `i686-pc-windows-msvc` | `package/win-x86/` |

The quick start uses the 64-bit target. For 32-bit Excel, replace the target in
its build command and load the XLL from `win-x86` instead.

## Install the Windows build tools

You need:

- Windows 10 or Windows 11 and desktop Excel;
- Rust installed through rustup;
- Visual Studio Build Tools with the **Desktop development with C++** workload;
- Git, if you will clone the source repository.

This source snapshot uses Rust 1.98.1. Install it and your selected target:

```powershell
rustup toolchain install 1.98.1 --profile minimal
rustup target add --toolchain 1.98.1 x86_64-pc-windows-msvc
```

Use `i686-pc-windows-msvc` in the second command for 32-bit Excel. If Cargo
cannot find the MSVC linker, run the build from the Developer PowerShell supplied
by Visual Studio Build Tools.

## Obtain the source checkout

The quick start uses a local checkout so that the framework and packaging tool
come from the same source version. In the directory where you keep projects:

```powershell
git clone https://github.com/nakashima-hikaru/xlfn.git xlfn
```

If you already have a checkout named `xlfn`, use its parent directory instead of
cloning again. Stay in that parent directory: the tutorial creates `hello-xlfn`
beside `xlfn` and installs `cargo-xlfn` from `xlfn/crates/cargo-xlfn`.

The source version documented here is `0.2.0`. A crates.io release may not yet
be available; the local path used by the quick start does not depend on one.

## Using a published release instead

When the matching release is available on crates.io, you can install the CLI
without a checkout:

```powershell
cargo +1.98.1 install cargo-xlfn --version 0.2.0 --locked
```

In your add-in's `Cargo.toml`, use the published dependency instead of the path:

```toml
[dependencies]
xlfn = "=0.2.0"
```

Follow the remaining quick-start steps with the same `cdylib` target and source
code. Skip its `cargo install --path ...` command. Check the release's own
toolchain requirements when choosing a different version.

## Optional capabilities

The first add-in needs no Cargo features. Enable a feature when you reach a
guide that uses it:

| Feature | Use it for |
| --- | --- |
| `async` | a formula that completes asynchronously |
| `handles` | Rust objects owned by worksheet formulas |
| `rtd` | a formula that receives repeated updates |
| `cache` | calculations shared across cells |

For example, a local checkout with async support uses:

```toml
[dependencies]
xlfn = { path = "../xlfn/crates/xlfn", features = ["async"] }
```

`handles` and `rtd` are independent. Async handle inputs need both `async` and
`handles`. The `refinement` and `bench-internals` features are for repository
verification, not add-in development.

## Other platforms and Excel versions

Portable Rust tests can run on other operating systems. Producing and loading
the Windows XLL requires the Windows MSVC tools and Excel. xlfn uses the Excel
12 C API; native async functions use Excel 2010 or later as their operational
baseline. Exact Windows and Excel support is established for each release, as
described in the [compatibility reference](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/reference/compatibility.md).

Continue with [Create your first add-in](quick-start.md).
