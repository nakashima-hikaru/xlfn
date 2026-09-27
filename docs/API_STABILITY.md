# API stability and release qualification

This document records the proposed 1.0 contract and release-review boundaries.
It is for maintainers preparing the stable release.

## Contract intended for 1.0

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

## Calculation cache non-guarantees

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
