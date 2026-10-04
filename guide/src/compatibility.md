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

## Public API cleanup in this checkout

The current pre-1.0 cleanup requires these source migrations. No compatibility
aliases are retained for removed wrappers, paths, or method names.

| Previous spelling | Current spelling |
| --- | --- |
| `ExcelErrorValue(ExcelError::NotAvailable)` | `ExcelError::NotAvailable` for inputs, outputs, and RTD values |
| `value::raw::*`, `value::borrowed::*`, `value::matrix::*`, `value::date::*` | The corresponding `value::*` imports |
| `diagnostics::event::*`, `diagnostics::id::DiagnosticId` | The corresponding `diagnostics::*` imports |
| `xlfn::XlValueType` | `xlfn::value::XlValueType` |
| `XlStrRef::to_string()` | `XlStrRef::try_to_string()` |
| Tuple returned by `XlArrayRef::shape()` | `error::Shape { rows, columns }` |
| `endpoint.get(&registry, key)` | `registry.get(&endpoint, key)` |
| `endpoint.get_or_try_insert(&registry, key, weight, compute)` | `registry.get_or_try_insert(&endpoint, key, weight, compute)` |
| `RtdCapacity::from_usize(n)` | `RtdCapacity::disabled_if_zero(n)`; zero disables admission |

`ExcelError` and `XlValueType` now require a fallback arm in application matches.
Framework-owned structured `XllError` variants can be inspected with `..`, but
cannot be constructed by applications. Use `XllError::custom(excel, message)`
for an application failure with diagnostic text and a chosen worksheet error.
`Result<T, ExcelError>`, I/O errors, and diagnostic initialization errors have
explicit boundary conversions.

Matching execution flags and context modes may be repeated; conflicting modes
remain compile errors. `OpenResult<Self>` abbreviates the add-in open result.
Layer tuples are documented by `execution::UdfLayers`.

The input-only value types retain missing and blank semantics. Return shapes
remain explicit through `Row`, `Column`, or `Matrix`; there is no implicit
`Vec` orientation, `Option::None` return, or blank-to-empty-string conversion.
Finite numeric output support now also includes `f32`, small integer types,
`u32`, `u64`, `usize`, and `isize`. Wide integers reject values outside the
exact binary64 integer range rather than rounding silently.

`CalculationCache` owns one typed cache, while `CacheRegistry` owns multiple
lazy typed caches. Their lookup results differ because registry resolution can
fail. Weight callbacks remain supplied per initialization, allowing a caller
to capture computation-specific resource costs; each cache must still use one
consistent weight measure. An add-in implementation and its lifecycle state
remain explicit, and context types retain their standard `AsRef` integration.
