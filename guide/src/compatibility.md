# Feature and compatibility reference

This chapter distinguishes implemented targets from environments that have been independently qualified. “Builds” and “validated in Excel” are different claims.

## Crate and language baseline

For the source version documented by this guide:

| Item                            | Value                 |
| ------------------------------- | --------------------- |
| supported `xlfn` facade version | `0.2.0`               |
| Rust edition                    | 2024                  |
| minimum/pinned Rust toolchain   | `1.98.1`              |
| license                         | MIT OR Apache-2.0     |
| Excel C API generation          | Excel 12 / `XLOPER12` |

The supported application contract is the `xlfn` facade. The implementation
crates `xlfn-common`, `xlfn-kernel`, and `xlfn-macros` intentionally use an
independent `0.x` versioning domain and are not direct application APIs.
`xlfn-sys`, `xlfn-package`, and `cargo-xlfn` are evaluated separately because
their ABI, packaging, and CLI contracts are distinct from the facade.

The repository pins the toolchain and both Windows MSVC targets in `rust-toolchain.toml`. Downstream applications should record their actual compiler and `cargo-xlfn` version in release evidence.

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

Exact support for a particular Microsoft 365 channel, perpetual Excel build, locale, and organizational security configuration must be established by the release qualification matrix. See [Testing and release qualification](testing.md).

## Qualification status

The repository contains automated Windows artifact checks and a real-Excel release-gate procedure. The [release readiness record](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/RELEASE_READINESS.md) tracks the evidence and remaining gates for the 1.0 candidate. It does **not** claim completed real-Excel validation for all Windows 10/11 and 32/64-bit combinations.

Accordingly:

- the two MSVC architectures are implemented build targets;
- PE/export/import validation can be automated;
- production support claims must be based on recorded execution of the real-Excel matrix for the release candidate;
- downstream distributors should publish their own tested Excel versions and channels.

Do not convert an intended target into a support claim without evidence.

## xlfn features

The `xlfn` crate has no default features.

| Feature           | Adds                                                                       | Use when                                                        |
| ----------------- | -------------------------------------------------------------------------- | --------------------------------------------------------------- |
| `async`           | native async UDF executor, async context, calculation cancellation exports | a formula produces one eventual result without blocking Excel   |
| `handles`         | formula-owned typed objects, aliases, and scoped handle inputs              | a worksheet formula owns a Rust object                           |
| `rtd`             | typed streaming sources, subscriptions, and RTD configuration               | a formula receives repeated updates from a push source           |
| `cache`           | concurrent calculation cache and endpoints                                 | an add-in shares or bounds internal computation across cells    |

`handles` and `rtd` share a private Excel RTD transport, but neither enables the
other's public API. Async handle inputs need both `async` and `handles`.
`refinement` and `bench-internals` are repository verification facilities,
outside the supported application API. To use all supported capabilities,
declare `features = ["async", "handles", "rtd"]` on the `xlfn` dependency.

Examples:

```toml
[dependencies]
xlfn = "0.2"
```

```toml
[dependencies]
xlfn = { version = "0.2", features = ["async"] }
```

Qualify every feature combination that you distribute. Async changes the expected export set;
bundle contents and application-adapter dependencies have separate packaging and trust requirements.

## Raw Excel ABI access

The `xlfn` crate does not expose the raw Excel ABI. Applications that
intentionally need raw ABI types or calls should declare `xlfn-sys` directly:

```toml
[dependencies]
xlfn-sys = "0.2"
```

```rust
use xlfn_sys::XLOPER12;
```

Generated code may use hidden items under `xlfn::__private`, but that module is
an implementation detail and is not a supported application API.

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

## Source and binary compatibility

The `0.x` line is pre-1.0. Treat public Rust APIs, macro diagnostics, package metadata, and generated artifacts as subject to intentional breaking change between minor releases. Pin versions for production builds and review release notes before upgrading.

### Contract intended for 1.0

The version in this checkout is still `0.2.0`. The following defines the scope
to freeze when 1.0 is released; it does not announce that release:

- The documented `xlfn` facade, including `xlfn::output`, `xlfn::cache`, its prelude, macro inputs and generated behavior,
  and the `async`, `cache`, `handles`, and `rtd` feature APIs form the stable application
  contract. Removing or incompatibly changing them requires a major release.
- Custom conversion, lifecycle, execution-layer, and RTD extension traits are
  included. Adding a required trait method or changing a public type's fields,
  exhaustive variants, lifetimes, or thread-safety bounds must be reviewed for
  downstream source compatibility.
- Hidden macro support, benchmark helpers,
  and refinement trace formats are excluded. Code opting into these facilities
  must pin the exact framework version. Experimental features are not implied
  by the supported feature set.
- Macro error wording, rustc diagnostic formatting, backtraces, log prose,
  timing, allocation strategy, and private handle-token text are not stable
  formats. Documented errors and capability/lifetime restrictions remain part
  of the contract. Handles are session-scoped and must not be persisted.
- Rust has no stable binary ABI here. Rebuild the complete XLL and its generated
  wrappers together when updating the framework; do not mix compiled Rust
  objects from different versions. Workbook-visible names and semantics remain
  the add-in author's responsibility.
- Rust `1.98.1` is the initial minimum toolchain. A minimum-version increase
  must be documented and made in a minor or major release, not a patch release.
  Qualified Windows/Excel environments are recorded separately below.

### Calculation cache non-guarantees

The following internal operational characteristics are intentionally excluded from the stable contract and may change without notice:

- Eviction algorithm and eviction order
- Exact residency decisions and strict process-memory bounds
- Underlying cache backend implementation
- Maintenance cadence and thread assignment for destructors
- Reclamation mechanism, grace periods, and timing
- Number of internal shards or stripes
- Internal synchronization and single-flight coordination primitives

`xlfn-sys`, the programmatic `xlfn-package` API, and the `cargo-xlfn` CLI have
their own release contracts. The facade's 1.0 commitment does not implicitly
stabilize every implementation crate. See the maintainer's
[release procedure](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/RELEASING.md) for version alignment, artifact
formats, and the baseline update required before the first stable release.

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

| Environment                                   | Artifact check | Load/open | sync UDF | MTR | handles | async | RTD | external adapter | unload/reload |
| --------------------------------------------- | -------------- | --------- | -------- | --- | ------- | ----- | --- | ---------------- | ------------- |
| Windows 10, Excel 32-bit, exact build/channel |                |           |          |     |         |       |     |                  |               |
| Windows 10, Excel 64-bit, exact build/channel |                |           |          |     |         |       |     |                  |               |
| Windows 11, Excel 32-bit, exact build/channel |                |           |          |     |         |       |     |                  |               |
| Windows 11, Excel 64-bit, exact build/channel |                |           |          |     |         |       |     |                  |               |

Record failures and skipped capabilities explicitly; a blank cell must not be interpreted as a pass.
