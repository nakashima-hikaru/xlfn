# Glossary

**ABI (Application Binary Interface)** — The binary contract between compiled components: calling convention, symbol names, parameter widths, structure layout, alignment, ownership, and error protocol.

**Add-in generation** — One successful `xlAutoOpen` through the matching terminal `xlAutoClose`. State, handle tokens, async calculations, and RTD ownership are scoped to a generation.

**Async UDF** — An Excel native asynchronous worksheet function that returns control promptly and later supplies one final result through Excel's async handle.

**Bearer capability** — A value whose possession grants access. An xlfn handle token is a bearer capability: keep it unguessable and validate its type, authentication tag, and generation.

**Bitness** — The process architecture, x86 or x64. The XLL and every in-process binary dependency must match the Excel process, not merely the operating system.

**Calculation ID** — A runtime correlation identifier for one Excel calculation generation. It is not a durable workbook key.

**Calculation cache** — A concurrent, generation-aware application cache. Calling `clear` advances its internal generation so stale in-flight computations cannot repopulate the new generation; an application may choose to clear it at an Excel calculation boundary.

**Cache endpoint** — A typed, named view of a calculation cache. Its endpoint identity prevents unrelated key/value domains from colliding.

**Caller/formula identity** — Framework identity for the worksheet formula currently producing a formula-owned handle. It lets re-evaluation replace the object's value while preserving the formula's stable handle token.

**Cancellation guarantee** — The documented strength of cancellation for an async or application operation, such as guaranteed cancellation before start versus best-effort observation after start.

**COM** — Microsoft's Component Object Model. Excel RTD uses COM interfaces for connection, notification, refresh, and server lifetime management.

**Conversion boundary** — The generated wrapper point where a raw `XLOPER12` becomes a typed Rust parameter and a Rust result becomes framework-owned Excel return storage.

**`cdylib`** — A Rust library target that produces a C-compatible dynamic library. An xlfn add-in package must contain exactly one `cdylib` target.

**Diagnostic ID** — A stable, searchable numeric identifier attached to an internal failure. The worksheet receives a safe Excel error while operators use the ID to find detail.

**Excel reference** — An unevaluated cell or area reference received through macro-sheet capability. It is borrowed for the active call and may describe multiple areas.

**Excel-visible argument** — A parameter supplied by a worksheet formula. Injected contexts are not Excel-visible and do not consume Function Wizard arguments.

**Formula-owned handle** — An authenticated opaque string representing one formula-to-object ownership edge. Re-evaluation can reuse the binding, while an explicit alias can create another edge to the same underlying object.

**Function Wizard** — Excel's UI for discovering functions and displaying category, description, help topic, and argument metadata.

**Import closure** — The complete directed graph of DLL imports rooted at the XLL and every bundled PE sidecar, including delay imports.

**Main-thread context** — A capability available only during a main-thread UDF invocation, permitting selected Excel callbacks, handle publication, and RTD subscription.

**Macro-sheet capability** — An Excel registration mode required for receiving references and using APIs that are not allowed in ordinary thread-safe functions. It does not mean authoring an Excel 4 macro sheet.

**MTR (Multi-Threaded Recalculation)** — Excel's concurrent calculation engine. A `thread_safe` UDF may run simultaneously on Excel calculation threads and must avoid main-thread-only APIs.

**Adapter call gate** — An application-defined admission, serialization, or reentry policy in front of an external implementation. xlfn does not provide or select this policy.

**Owner/handle split** — A design in which a non-cloneable owner controls worker shutdown and resource destruction while cloneable handles submit bounded operations. This prevents worker ownership from escaping into jobs.

**Package** — One bitness-specific deployment directory containing the XLL, optional bundled sidecar files, and `build-manifest.json`.

**PE (Portable Executable)** — The Windows executable format used by XLLs and DLLs. `cargo xlfn` inspects PE architecture, exports, imports, and delay imports.

**Quiescence** — The state in which no operation can still execute code or callbacks belonging to a subsystem. `xlAutoClose` must establish quiescence before Excel can unload the XLL.

**RTD (Real-Time Data)** — Excel's streaming update mechanism. A source creates a subscription, publishes repeated scalar values through a sink, and synchronously disconnects during shutdown.

**RTD topic** — An ordered sequence of stable string parts identifying one stream within a source.

**Rustdoc** — Generated API documentation from Rust items and doc comments. The user guide explains workflows and design; rustdoc provides exhaustive signatures and per-item details.

**Stale handle** — A structurally valid handle token whose add-in generation or object generation is no longer live.

**System import policy** — The versioned list/rules that classify standard Windows DLLs as system-provided during package import validation.

**Thread-affine state** — A resource that must be created, called, and destroyed on one OS thread. Preserving this requirement is the responsibility of the application adapter; xlfn does not provide an external-engine owner/worker abstraction.

**Thread-safe context** — A capability for an MTR-safe UDF. It exposes shared add-in state but not main-thread-only Excel operations.

**UDF (User-Defined Function)** — A worksheet function implemented by the XLL and registered with Excel.

**UDF layer** — Bounded middleware around every UDF invocation for admission control and instrumentation. Layers see call metadata before conversion and a classified outcome afterward.

**Volatile function** — A function Excel recalculates whenever a relevant recalculation occurs, even when explicit arguments appear unchanged. Volatility should be used sparingly.

**Worker health** — Application-defined state for an adapter worker or pool, used to distinguish running, closing, failed, and stopped execution resources. xlfn does not define this state.

**XLL** — An Excel native add-in: a Windows DLL with Excel-defined lifecycle, registration, callback, and memory-management exports.

**`XLOPER12`** — The Excel 12 C API value structure used to exchange numbers, strings, errors, references, arrays, async handles, and other worksheet values.
