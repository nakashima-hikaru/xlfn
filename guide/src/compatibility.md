# Feature and compatibility reference

This chapter distinguishes implemented targets from environments that have been independently qualified. “Builds” and “validated in Excel” are different claims.

## Crate and language baseline

For the source version documented by this guide:

| Item | Value |
|---|---|
| workspace version | `0.1.0` |
| Rust edition | 2024 |
| minimum/pinned Rust toolchain | `1.97.1` |
| license | MIT OR Apache-2.0 |
| Excel C API generation | Excel 12 / `XLOPER12` |

The repository pins the toolchain and both Windows MSVC targets in `rust-toolchain.toml`. Downstream applications should record their actual compiler and `cargo-xlfn` version in release evidence.

## Runtime targets

The supported XLL target implementations are:

| Excel process | Rust target | Distribution directory |
|---|---|---|
| 32-bit Excel | `i686-pc-windows-msvc` | `win-x86` |
| 64-bit Excel | `x86_64-pc-windows-msvc` | `win-x64` |

Select by **Excel process bitness**, not Windows bitness. A 64-bit Windows installation may run 32-bit Excel and therefore require the x86 package.

The intended operating-system baseline is Windows 10 or Windows 11 with the MSVC toolchain. Non-Windows hosts may run portable unit tests and inspect source, but they do not produce a runnable Excel XLL without the Windows target toolchain and linker environment.

## Excel versions

Synchronous `XLOPER12` functions target Excel versions that support the Excel 12 C API. Native asynchronous UDFs rely on Excel's async ABI; use Excel 2010 or later as the operational baseline for the `async` feature.

Exact support for a particular Microsoft 365 channel, perpetual Excel build, locale, and organizational security configuration must be established by the release qualification matrix. See [Testing and release qualification](testing.md).

## Qualification status

The repository contains automated Windows artifact checks and a real-Excel release-gate procedure. At the source snapshot used for this guide, the existing implementation-status record does **not** claim completed real-Excel validation for all Windows 10/11 and 32/64-bit combinations.

Accordingly:

- the two MSVC architectures are implemented build targets;
- PE/export/import validation can be automated;
- production support claims must be based on recorded execution of the real-Excel matrix for the release candidate;
- downstream distributors should publish their own tested Excel versions and channels.

Do not convert an intended target into a support claim without evidence.

## Facade features

The `xlfn` facade has no default features.

| Feature | Adds | Use when |
|---|---|---|
| `async` | native async UDF executor, async context, calculation cancellation exports | a formula produces one eventual result without blocking Excel |

Examples:

```toml
[dependencies]
xlfn = "0.1"
```

```toml
[dependencies]
xlfn = { version = "0.1", features = ["async"] }
```

Qualify every feature combination that you distribute. Async changes the expected export set;
bundle contents and application-adapter dependencies have separate packaging and trust requirements.

## Build-profile requirements

The framework catches panics at XLL boundaries and relies on unwinding behavior. Release profiles must use:

```toml
[profile.release]
panic = "unwind"
```

Do not switch an add-in to `panic = "abort"`; a panic would terminate Excel rather than being converted to a worksheet error and diagnostic event.

## Crate-name requirement

Procedural macro output currently refers to the facade as `::xlfn`. Use the canonical dependency name:

```toml
[dependencies]
xlfn = "0.1"
```

Do not rename it with a dependency alias unless the macro implementation for the selected release explicitly documents alias resolution.

## Source and binary compatibility

Version `0.1.x` is pre-1.0. Treat public Rust APIs, macro diagnostics, package metadata, and generated artifacts as subject to intentional breaking change between minor releases. Pin versions for production builds and review release notes before upgrading.

Workbook compatibility is a separate concern. The following are workbook-visible public API:

- Excel function names;
- argument order and presence policy;
- accepted enum strings;
- error semantics;
- calculation behavior;
- handle-producing versus scalar-producing behavior;
- stable UDF IDs where identity affects runtime state.

Use additive changes where possible. Rename or remove a published worksheet function only through an explicit workbook migration plan. xlfn does not require retaining Rust compatibility shims inside a developing add-in, but deployed workbook contracts still need operational governance.

## External component compatibility

When the application uses an external binary component, its adapter must account for:

- Excel process bitness;
- PE machine type for the XLL and every bundled DLL;
- exact calling convention and symbol spelling;
- any selected ABI's layout, packing, scalar widths, ownership, and error protocol;
- any application-defined protocol or ABI version negotiation;
- the transitive import policy;
- thread-affinity and concurrency guarantees.

xlfn does not perform runtime adapter loading or ABI negotiation. Any application-defined probe occurs according to the chosen adapter and is not a pre-execution security boundary for in-process code that has already been loaded.

## Support matrix template

Publish a matrix for each release candidate:

| Environment | Artifact check | Load/open | sync UDF | MTR | handles | async | RTD | external adapter | unload/reload |
|---|---|---|---|---|---|---|---|---|---|
| Windows 10, Excel 32-bit, exact build/channel |  |  |  |  |  |  |  |  |  |
| Windows 10, Excel 64-bit, exact build/channel |  |  |  |  |  |  |  |  |  |
| Windows 11, Excel 32-bit, exact build/channel |  |  |  |  |  |  |  |  |  |
| Windows 11, Excel 64-bit, exact build/channel |  |  |  |  |  |  |  |  |  |

Record failures and skipped capabilities explicitly; a blank cell must not be interpreted as a pass.
