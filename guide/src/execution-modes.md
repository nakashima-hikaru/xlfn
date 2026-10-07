# Execution modes and contexts

Choose an execution mode based on the Excel operations your function needs.
For a pure calculation, use the default main-thread mode or declare
`thread_safe` when the entire call path supports concurrent execution.

A context gives a function access to [application state](lifecycle.md) and
the operations allowed in that mode. It is optional for functions that need
neither state nor those operations.

## Compatibility table

| Mode | How selected | Excel MTR | Can use raw references | Can return a new handle object |
|---|---|---:|---:|---:|
| Main thread | default (no context) or `MainThreadContext` | no | no | yes |
| Thread-safe | synchronous function with `thread_safe` and no context, or `ThreadSafeContext` | yes | no | no |
| Macro-sheet | synchronous function with `macro_sheet` and no context, or `MacroSheetContext` | no | yes | no |
| Asynchronous | `async fn` | native async ABI | no | no |


## Add a context when needed

A context must be the first parameter and be passed by value. The macro
recognizes the standard context type in the signature and injects it; the
context does not appear as a worksheet argument. For a synchronous function,
`MainThreadContext`, `ThreadSafeContext`, and `MacroSheetContext` select their
respective modes. `async fn` selects asynchronous execution and may accept
`AsyncContext` when it needs state or cancellation.

Functions with a context must omit both `thread_safe` and `macro_sheet` from
`#[excel_function]`, including a flag that matches the context's mode. A
synchronous function cannot accept `AsyncContext`, and an async function
cannot accept any synchronous context. Async functions must also omit both
mode flags, whether or not a context is present.

Use the standard type name directly, such as
`ThreadSafeContext<'_, AppTools>` after an ordinary import, or a qualified
path such as `xlfn::ThreadSafeContext<'_, AppTools>`. The macro recognizes the
path's final identifier. Type aliases, renamed imports, and custom context
types are not supported for injection.

The examples below assume `use xlfn::prelude::*;` and an add-in named
`AppTools` whose shared state contains the fields being read. See
[Share application state](lifecycle.md) for the add-in definition.

## Main-thread context

```rust
{{#include ../fixtures/app.md}}
#[excel_function(name = "APP.ENVIRONMENT")]
fn environment(
    context: MainThreadContext<'_, AppTools>,
) -> String {
    context.state().environment.clone()
}
```

`MainThreadContext` is neither `Send` nor `Sync`. It has one inferred lifetime tied to the current Excel-call scope; the context keeps application state available for the duration of the call. With the `rtd` feature, `context.rtd()` returns the narrower RTD capability that establishes a streaming subscription. Formula-owned object producers use main-thread return semantics, even when they do not explicitly request a context.

## Thread-safe context

```rust
{{#include ../fixtures/app.md}}
#[excel_function(name = "APP.VERSION")]
fn version(
    context: ThreadSafeContext<'_, AppTools>,
) -> String {
    context.state().version.clone()
}
```

`ThreadSafeContext` selects thread-safe execution. A function without a context
can select that mode with `#[excel_function(thread_safe)]`.

### What `thread_safe` guarantees

`thread_safe` declares that Excel may invoke the generated boundary concurrently on calculation threads. It does not make `State` or anything reached through `State` thread-safe. Every application resource used by the function must independently support concurrent access or be protected by an application synchronization or dispatch policy.

Do not move call-scoped Excel values, raw references, or callback capabilities to another thread. Thread affinity below the Rust worksheet-function boundary is a separate application concern from Excel's execution mode.

## Macro-sheet context

```rust
{{#include ../fixtures/app.md}}
use xlfn::reference::ExcelReference;

#[excel_function(name = "APP.RANGE.NAME")]
fn range_name(
    context: MacroSheetContext<'_, AppTools>,
    #[excel_arg(reference)] reference: ExcelReference<'_>,
) -> XllResult<String> {
    context.sheet_name(&reference)
}
```

A macro-sheet context permits Excel callback operations that are not allowed in thread-safe functions. It is neither `Send` nor `Sync`; its one inferred lifetime is the current Excel-call scope. The generated Excel entrypoint supplies its callback authority; creating an input-memory scope cannot create or reset it. Context clones share the same session. Once Excel returns `Abort` or `Uncalced`, later callbacks through every context for that invocation are suppressed. It provides:

- `coerce` for an owned `ExcelValue`;
- `coerce_matrix<T>` for an owned matrix;
- `sheet_name`.

The `macro_sheet` flag selects macro-sheet registration for a function without
a context. Macro-sheet mode is incompatible with `thread_safe` and asynchronous
functions.

## Asynchronous context

```rust
{{#include ../fixtures/app.md}}
#[excel_function(name = "APP.SLOW")]
async fn slow(
    context: AsyncContext<'_, AppTools>,
    input: String,
) -> XllResult<String> {
    context.check_cancelled()?;
    Ok(input)
}
```

The framework-owned future retains the current open-generation lease and per-call cancellation token. `AsyncContext<'_, AppTools>` borrows those capabilities for the invocation, so it is available only with the `async` feature and only to `async fn`; it cannot escape into a detached task. An async function may omit the context if it does not need state or cancellation.

A function marked `volatile` must still return a type valid for its mode. Handle objects and `HandleAlias<'_, T>` support volatile main-thread return semantics. Borrowed `Handle<'_, T>` values are synchronous call-scoped inputs and cannot be used in async functions. An async function that needs a formula-owned object must use the generation-scoped `HandleLease<'_, T>` input, which pins the registry payload before the task is committed.

## Next steps

- See [Errors and diagnostics](errors-diagnostics.md) for returning Excel errors and configuring logs.
- See [Asynchronous functions](async-functions.md) for non-blocking calculations.
- Refer to [docs.rs](https://docs.rs/xlfn) for full context and trait definitions.


A reference's `areas()` iterator reports its remaining length through `len()`.
Every area is validated before the reference is exposed. `row_count()` and
`column_count()` include both endpoints; `cell_count()` returns `u64` so a full
worksheet fits on 32-bit hosts. Formatting an area produces a sheet-local A1
address such as `A1:B10`, without a sheet name or absolute-reference markers.
