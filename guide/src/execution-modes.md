# Execution modes and contexts

xlfn separates Excel-visible arguments from injected capabilities. A context, when present, must be the first parameter and must be passed by value with exactly one `#[excel_context(...)]` role.

## Main-thread context

```rust
#[excel_function(name = "APP.DESK")]
fn desk(
    #[excel_context(main_thread)] context: MainThreadContext<'_, '_, State>,
) -> String {
    context.state().desk.clone()
}
```

`MainThreadContext` is neither `Send` nor `Sync`. Its two inferred lifetimes are the state borrow and the current Excel-call scope; the scope lifetime keeps callback capability tied to the invocation. It gives access to state and to `subscribe`, which establishes a streaming RTD dependency. Formula-owned object producers also use main-thread return semantics, even when they do not explicitly request a context.

Do not combine a main-thread context with `thread_safe`.

## Thread-safe context

```rust
#[excel_function(name = "APP.VERSION", thread_safe)]
fn version(
    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, State>,
) -> String {
    context.state().version.clone()
}
```

`ThreadSafeContext` is `Copy`, `Send`, and `Sync` when the referenced state permits it. Its presence marks the function as thread-safe even if the function attribute omits the flag. Use an explicit attribute as well when it improves readability, but do not treat duplicate declaration as additional safety.

## Macro-sheet context

```rust
#[excel_function(name = "APP.RANGE.NAME")]
fn range_name(
    #[excel_context(macro_sheet)] context: MacroSheetContext<'_, '_, State>,
    #[excel_arg(reference)] reference: ExcelReference<'_>,
) -> XllResult<String> {
    context.sheet_name(&reference)
}
```

A macro-sheet context permits Excel callback operations that are not allowed in thread-safe functions. It is neither `Send` nor `Sync`; like `MainThreadContext`, its second inferred lifetime is the current Excel-call scope. It provides:

- `coerce` for an owned `OwnedExcelValue`;
- `coerce_matrix<T>` for an owned matrix;
- `sheet_name`.

The `macro_sheet` function flag selects the same registration capability without injecting state access. It is incompatible with `thread_safe` and asynchronous functions.

## Asynchronous context

```rust
#[excel_function(name = "APP.SLOW")]
async fn slow(
    #[excel_context(asynchronous)] context: AsyncContext<State>,
    input: String,
) -> XllResult<String> {
    context.check_cancelled()?;
    Ok(input)
}
```

`AsyncContext` owns an `Arc<State>` and a per-call cancellation token. It is available only with the `async` feature and only to `async fn`. An async function may omit the context if it does not need state or cancellation.

## Compatibility table

| Mode | How selected | Excel MTR | Can use raw references | Can return a new handle object |
|---|---|---:|---:|---:|
| Main thread | default or `main_thread` context | no | no | yes |
| Thread-safe | `thread_safe` or `thread_safe` context | yes | no | no |
| Macro-sheet | `macro_sheet` or `macro_sheet` context | no | yes | no |
| Asynchronous | `async fn` | native async ABI | no | no |

A function marked `volatile` must still return a type valid for its mode. Handle objects and `Handle<T>` aliases support volatile main-thread return semantics.
