# Feature and compatibility reference

Use this page to choose dependencies and check which Windows and Excel
environments your add-in needs to support.

## Crate and language baseline

For the source version documented by this guide:

| Item                            | Value                 |
| ------------------------------- | --------------------- |
| `xlfn` version                | `0.2.0`               |
| Rust edition                    | 2024                  |
| minimum/pinned Rust toolchain   | `1.98.1`              |
| license                         | MIT OR Apache-2.0     |
| Excel C API generation          | Excel 12 / `XLOPER12` |

The repository pins the toolchain and both Windows MSVC targets in
`rust-toolchain.toml`. Record the compiler and `cargo-xlfn` version used for
your add-in package.

## Runtime targets

The supported XLL target implementations are:

| Excel process | Rust target              | Package directory |
| ------------- | ------------------------ | ----------------- |
| 32-bit Excel  | `i686-pc-windows-msvc`   | `win-x86`         |
| 64-bit Excel  | `x86_64-pc-windows-msvc` | `win-x64`         |

Select by **Excel process bitness**, not Windows bitness. A 64-bit Windows installation may run 32-bit Excel and therefore require the x86 package.

The intended operating-system baseline is Windows 10 or Windows 11 with the MSVC toolchain. Non-Windows hosts may run portable unit tests and inspect source, but they do not produce a runnable Excel XLL without the Windows target toolchain and linker environment.

## Excel versions

Synchronous `XLOPER12` functions target Excel versions that support the Excel 12 C API. Native asynchronous UDFs rely on Excel's async ABI; use Excel 2010 or later as the operational baseline for the `async` feature.

Test the Excel versions, channels, and locales you plan to support. Use
[Test in Excel before release](../EXCEL_TESTING.md) to record the results.

## xlfn features

The `xlfn` crate has no default features.

| Feature           | Adds                                                                       | Use when                                                        |
| ----------------- | -------------------------------------------------------------------------- | --------------------------------------------------------------- |
| `async`           | native async UDF executor, async context, calculation cancellation exports | a formula produces one eventual result without blocking Excel   |
| `handles`         | formula-owned typed objects, aliases, and scoped handle inputs              | a worksheet formula owns a Rust object                           |
| `rtd`             | typed streaming sources, subscriptions, and RTD configuration               | a formula receives repeated updates from a push source           |
| `cache`           | concurrent calculation cache and endpoints                                 | an add-in shares or bounds internal computation across cells    |

Async handle inputs need both `async` and `handles`. To enable all four
capabilities, declare `features = ["async", "cache", "handles", "rtd"]`.

Examples:

```toml
[dependencies]
xlfn = "0.2"
```

```toml
[dependencies]
xlfn = { version = "0.2", features = ["async"] }
```

Test each feature combination you distribute in Excel.

## Build-profile requirements

The framework catches panics at XLL boundaries and relies on unwinding behavior. Release profiles must use:

```toml
[profile.release]
panic = "unwind"
```

Do not switch an add-in to `panic = "abort"`; a panic would terminate Excel rather than being converted to a worksheet error and diagnostic event.

## Dependency names

Procedural macros resolve the framework's dependency name from `Cargo.toml`. Both the canonical name and a dependency alias are supported:

```toml
[dependencies]
my_xlfn = { package = "xlfn", version = "0.2" }
```

Use `my_xlfn::prelude::*` with this declaration. When accessing the framework through a Rust re-export, override resolution with `crate = "path"` on the relevant macro; see the [attribute reference](attributes.md).

## Upgrading an add-in

The `0.x` line may change between minor releases. Pin the framework version and review release notes when upgrading.

### Changes in this source revision

Applications moving to this source revision should make these configuration
changes:

| Previous API | Current API |
| --- | --- |
| `AsyncRuntimeConfig` | `AsyncConfig` |
| `.with_handle_config(config)` | `.with_handles(config)` |
| `.with_rtd_limits(limits)` | `.with_rtd(RtdConfig::new().with_limits(limits))` |
| `.with_async_worker_count(count)` | `.with_async(AsyncConfig::new().with_worker_count(count))` |

The old names are removed. Each capability is configured through its own
configuration type and attached to `RuntimeConfig` with the corresponding
`with_*` method.

Non-finite numerical results now consistently produce `#NUM!`, including
matrix, row, column, array-builder and custom `IntoExcel` outputs. These paths
previously could report `#VALUE!` while a scalar `f64` reported `#NUM!`. Review
workbook formulas that distinguish these errors. Input validation is unchanged.

`#[excel_arg(reference)]` now dispatches to the declared argument type's
`FromExcelReference` implementation, allowing both owned and borrowed custom
conversions. Reference arguments still cannot participate in a function that
creates a new handle; custom conversion does not relax that identity rule.

### Workbook compatibility

Existing workbooks depend on:

- Excel function names;
- argument order and presence policy;
- accepted enum strings;
- error semantics;
- calculation behavior;
- handle-producing versus scalar-producing behavior;
- stable UDF IDs where identity affects runtime state.

When changing a published worksheet function, test existing workbooks or plan
a migration for them.
