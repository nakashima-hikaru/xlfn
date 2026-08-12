# Requirements and compatibility

## Build host

Release XLLs target the Microsoft Visual C++ ABI and are expected to be built and linked on Windows with:

- Windows 10 or Windows 11;
- Rust 1.97.1 or a compatible toolchain for this source snapshot;
- the `i686-pc-windows-msvc` and/or `x86_64-pc-windows-msvc` Rust targets;
- Visual Studio Build Tools with **Desktop development with C++**;
- Cargo and `cargo-xlfn`.

The repository pins the toolchain and both targets in `rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.97.1"
profile = "minimal"
components = ["clippy", "rustfmt"]
targets = ["i686-pc-windows-msvc", "x86_64-pc-windows-msvc"]
```

Host-side tests that do not link an XLL may run on other operating systems. Packaging and release qualification still require Windows MSVC artifacts and real Excel.

> **Source-snapshot installation:** this repository currently sets `publish = false` for the workspace. Until an official crates.io release is published, install `cargo-xlfn` from an audited Git revision or local checkout and replace generated `version = "0.2"` dependencies with the same Git revision or a local `path`. The version-based dependency examples in this guide show the intended form for a published release.

## Excel bitness

Match the XLL to the **Excel process**, not to the operating system:

| Excel process | Rust target | Package directory |
|---|---|---|
| 32-bit Excel | `i686-pc-windows-msvc` | `package/win-x86/` |
| 64-bit Excel | `x86_64-pc-windows-msvc` | `package/win-x64/` |

A 64-bit edition of Windows can run 32-bit Excel. In that case, use the x86 XLL.

## Excel API level

xlfn uses the Excel 12/XLOPER12 interface. Asynchronous worksheet functions use Excel's native asynchronous UDF ABI and are intended for Excel versions that provide that ABI; the project documentation uses Excel 2010 or later as the operational baseline for this feature.

Do not convert this implementation target into an unqualified compatibility claim. Qualify each release candidate against the exact Excel channels, bitnesses, and Windows versions that you intend to support.

## Rust crate features

The facade crate has no default features:

```toml
[dependencies]
xlfn = "0.2"
```

Enable only what the add-in uses:

```toml
[dependencies]
xlfn = { version = "0.2", features = ["async"] }
```

| Feature | Adds |
|---|---|
| `async` | native asynchronous UDF runtime, `AsyncContext`, cancellation tokens, and calculation-event exports |

## Project shape

An XLL package must contain exactly one `cdylib` target and exactly one crate-root type attributed with `#[excel_addin]`. The generated lifecycle and COM exports are part of that definition. Do not hand-write competing `xlAutoOpen`, `xlAutoClose`, `xlAutoFree12`, `DllGetClassObject`, or related exports.
