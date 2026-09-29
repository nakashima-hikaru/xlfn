# Compatibility and versioning

xlfn is currently pre-1.0: upgrading to a new minor version can require source
changes. This chapter defines the intended 1.0 compatibility boundary; it is
not an announcement that 1.0 has shipped or completed Excel qualification.

## Application-facing contracts

The 1.0 contract covers the documented `xlfn` API, its re-exported macros and
their accepted attributes, and the `async`, `handles`, `rtd`, and `cache`
features. It includes `output`, conversion traits, lifecycle hooks, execution
layers, diagnostics, and RTD source/channel extension points. Features can be
enabled independently or in combination.

Compatible releases preserve documented conversion rules, Excel error mappings,
execution restrictions, cancellation and shutdown guarantees, and ownership
lifetimes. A Rust signature check alone does not establish these properties.
Changes to those contracts require regression tests and a compatibility review.
Exhaustive enums remain exhaustive; enums marked `#[non_exhaustive]` require a
fallback when matched by application code.

Custom `FromExcel` and `IntoExcel` implementations must follow their documented
conversion contracts. Handle-producing functions additionally use
`ExcelInputIdentity` and `PrepareExcel`: preparation validates all inputs and
records their complete semantic identity. A cache hit can skip materialization,
so validation or externally visible effects must not be deferred to that stage.
See the [API reference](https://docs.rs/xlfn) for each trait's requirements.

The supported deployment targets are Windows MSVC x86 and x64, matching the
Excel process bitness. The minimum Rust version is declared in `Cargo.toml`
and documented in [Requirements](requirements.md). A future increase must be
announced with the release; patch releases retain their compatibility line's
minimum Rust version. Portable tests on other operating systems do not establish
support for loading an XLL there.

## Implementation details

Items marked `#[doc(hidden)]`, `__private`, generated Rust identifiers,
`bench-internals`, and `refinement` are implementation interfaces. Do not call
or implement their hidden hooks directly. Identity encodings, runtime handle
tokens, diagnostic text, and refinement trace formats are not persistence
formats. Do not serialize handle tokens for use after an add-in generation or
Excel process ends.

`xlfn-common`, `xlfn-kernel`, and `xlfn-macros` are independently versioned support
crates. The facade pins their versions exactly where it depends on them. Add-ins
should use the macro re-exports from `xlfn`. `xlfn-sys` is the separate raw ABI
layer; direct use requires its unsafe contracts and does not inherit the safe
facade's compatibility or safety guarantees.

## Packaging and workbook compatibility

`cargo-xlfn` and `xlfn-package` have independent versions. Their documented CLI
options, Cargo metadata, and package validation behavior form their own
contracts. Human-readable command output is diagnostic text. Build manifests
carry an explicit schema version; consumers must check that version before
interpreting the fields. Unknown Cargo metadata settings are rejected to catch
configuration errors before packaging.

Framework updates cannot preserve workbooks if an add-in changes its own Excel
function names, argument ordering, defaults, enum spellings, or return shape.
Test saved workbooks when making those changes. Keep the add-in's worksheet
contract separate from its internal Rust implementation.

## Verification boundary

Rust tests, compile-fail tests, Miri, Loom, Lean, and Verus cover different
properties. The repository's formal models and shared protocol proofs do not
prove all native atomics, allocator behavior, operating-system synchronization,
COM, or Excel. Native refinement obligations remain explicitly documented in
the verification worklists. A release must retain those limits in its claims.

Every release candidate still needs Windows artifact/ABI tests and direct Excel
qualification for its exact built artifacts. Follow [Test your add-in](testing.md)
and [Deployment and distribution](deployment.md) for application testing.
