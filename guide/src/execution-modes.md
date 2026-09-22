# Execution modes and contexts

xlfn separates Excel-visible arguments from injected capabilities. A context, when present, must be the first parameter and must be passed by value with exactly one `#[excel_context(...)]` role.

## Main-thread context

```rust
#[excel_function(name = "APP.ENVIRONMENT")]
fn environment(
    #[excel_context(main_thread)] context: MainThreadContext<'_, AppTools>,
) -> String {
    context.state().environment.clone()
}
```

`MainThreadContext` is neither `Send` nor `Sync`. It has one inferred lifetime tied to the current Excel-call scope; the context keeps the open generation alive while exposing state and callback capability. With the `rtd` feature, `context.rtd()` returns the narrower RTD capability that establishes a streaming subscription. Formula-owned object producers use main-thread return semantics, even when they do not explicitly request a context.

Do not combine a main-thread context with `thread_safe`.

## Thread-safe context

```rust
#[excel_function(name = "APP.VERSION")]
fn version(
    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, AppTools>,
) -> String {
    context.state().version.clone()
}
```

The context parameter acts as the single source of truth for the function's execution mode. When `ThreadSafeContext` is present, it explicitly declares the function as thread-safe; do **not** declare `thread_safe` in `#[excel_function]`, as redundant mode declarations are rejected at compile time. The `#[excel_function(thread_safe)]` attribute flag is reserved for functions that do not take a context argument (i.e. pure computation UDFs that do not require state access).

### What `thread_safe` guarantees

`thread_safe` declares that Excel may invoke the generated boundary concurrently on calculation threads. It does not make `State` or anything reached through `State` thread-safe. Every application resource used by the function must independently support concurrent access or be protected by an application synchronization or dispatch policy.

Do not move call-scoped Excel values, raw references, or callback capabilities to another thread. Thread affinity below the Rust worksheet-function boundary is a separate application concern from Excel's execution mode.

## Macro-sheet context

```rust
#[excel_function(name = "APP.RANGE.NAME")]
fn range_name(
    #[excel_context(macro_sheet)] context: MacroSheetContext<'_, AppTools>,
    #[excel_arg(reference)] reference: ExcelReference<'_>,
) -> XllResult<String> {
    context.sheet_name(&reference)
}
```

A macro-sheet context permits Excel callback operations that are not allowed in thread-safe functions. It is neither `Send` nor `Sync`; its one inferred lifetime is the current Excel-call scope. It provides:

- `coerce` for an owned `ExcelValue`;
- `coerce_matrix<T>` for an owned matrix;
- `sheet_name`.

The `macro_sheet` attribute flag on `#[excel_function(macro_sheet)]` selects macro-sheet registration for pure functions without injecting state access. When `MacroSheetContext` is present in the parameters, it defines the execution mode, and adding `macro_sheet` to `#[excel_function]` is a compile error. Macro-sheet mode is incompatible with `thread_safe` and asynchronous functions.

## Asynchronous context

```rust
#[excel_function(name = "APP.SLOW")]
async fn slow(
    #[excel_context(asynchronous)] context: AsyncContext<'_, AppTools>,
    input: String,
) -> XllResult<String> {
    context.check_cancelled()?;
    Ok(input)
}
```

The framework-owned future retains the current open-generation lease and per-call cancellation token. `AsyncContext<'_, AppTools>` borrows those capabilities for the invocation, so it is available only with the `async` feature and only to `async fn`; it cannot escape into a detached task. An async function may omit the context if it does not need state or cancellation.

## Compatibility table

| Mode | How selected | Excel MTR | Can use raw references | Can return a new handle object |
|---|---|---:|---:|---:|
| Main thread | default (no context) or `main_thread` context | no | no | yes |
| Thread-safe | `thread_safe` flag (no context) or `thread_safe` context | yes | no | no |
| Macro-sheet | `macro_sheet` flag (no context) or `macro_sheet` context | no | yes | no |
| Asynchronous | `async fn` | native async ABI | no | no |

A function marked `volatile` must still return a type valid for its mode. Handle objects and `HandleAlias<'_, T>` support volatile main-thread return semantics. Borrowed `Handle<'_, T>` values are synchronous call-scoped inputs and cannot be used in async functions. An async function that needs a formula-owned object must use the generation-scoped `HandleLease<'_, T>` input, which pins the registry payload before the task is committed.
