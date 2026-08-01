# Introduction

xlfn is a Rust framework for building Excel XLL add-ins against the Excel 12/XLOPER12 C API. It keeps the Excel ABI, raw pointers, panic containment, return-value ownership, function registration, RTD transport, and lifecycle exports inside generated framework boundaries. Add-in authors work primarily with safe Rust functions and typed values.

A minimal exported function looks like ordinary Rust:

```rust
use xlfn::prelude::*;

#[excel_function(name = "EXAMPLE.ADD", thread_safe)]
pub fn add(left: f64, right: f64) -> f64 {
    left + right
}
```

The attribute generates the Excel ABI wrapper and registration descriptor. Arguments are converted through `FromExcel`; results are selected through `ExcelReturn` and converted through `IntoExcelValue`. Conversion behavior follows Rust trait resolution, so aliases and re-exports do not require macro-specific type-name recognition.

## What the framework provides

- one typed add-in lifecycle with `Addin::open`, `Addin::quiesce`, and `Addin::cleanup`;
- function registration generated from Rust attributes;
- strict scalar, string, error, array, date-serial, and reference conversion;
- main-thread, thread-safe, macro-sheet, and asynchronous capability contexts;
- formula-owned, type-checked object handles;
- native Excel asynchronous UDFs behind an optional feature;
- generic push-based RTD subscriptions;
- bounded diagnostic delivery and structured `tracing` events;
- typed loading and lifecycle tools for native DLLs;
- linked-artifact, PE architecture, export, and dependency validation;
- atomic x86/x64 distribution through `cargo xlfn`.

## What the framework does not infer

xlfn deliberately does not infer business semantics. In particular, it does not generate C declarations from headers, decide whether a native context is thread-safe, interpret status codes or output buffers, choose cache keys, or decide how an RTD source should cancel background work. Those contracts remain explicit in the adapter.

## Documentation map

Use this guide for workflows, mental models, constraints, and operational practices. Use generated rustdoc for exhaustive signatures:

```console
cargo doc --package xlfn --all-features --open
```

Start with [Requirements and compatibility](requirements.md), then complete [Create your first add-in](quick-start.md). Native-library users should first understand [Formula-owned handles](handles.md), [Asynchronous functions](async-functions.md), and the [Native integration overview](native-overview.md).

## Status and release claims

The repository distinguishes implemented behavior from real-Excel qualification. A successful Rust unit test or PE inspection is not, by itself, evidence that a release candidate has passed both 32-bit and 64-bit Excel scenarios. Before distributing a release, follow [Testing and release qualification](testing.md) and record the exact Windows and Excel environment used.
