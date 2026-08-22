# Attribute reference

This chapter is the compact reference for xlfn's procedural macros. The compiler validates incompatible combinations; prefer the smallest attribute set that accurately describes the Excel contract.

> **Dependency name:** generated code currently refers to the framework crate as `xlfn`. In `Cargo.toml`, depend on the package under its canonical crate name (`xlfn`) rather than renaming it.

## `#[excel_addin(...)]`

Place exactly one `#[excel_addin]` on a non-generic struct declared at the crate root:

```rust
#[excel_addin(
    name = "Desk Tools",
    id = "desk-tools",
    category = "DeskTools"
)]
pub struct DeskTools;
```

| Option | Meaning | Default |
|---|---|---|
| `name = "..."` | Display name shown by Excel's Add-in Manager | Rust struct name |
| `id = "..."` | Stable add-in identity used by runtime ownership and registration | lowercase Rust struct name |
| `category = "..."` | Default Function Wizard category | display name |

`name` and `category` must contain 1 through 255 UTF-16 code units. `id` must be a non-reserved ASCII slug of at most 64 bytes, begin with a letter, and contain only letters, digits, `-`, or `_`.

The macro emits the standard XLL lifecycle and COM exports. Implement [`Addin`](lifecycle.md) for the attributed type.

## `#[excel_function(...)]`

Apply the attribute to an ordinary or `async` free function:

```rust
/// Returns the present value of a cash flow.
#[excel_function(
    name = "MATH.SCALE",
    id = "math_scale_v1",
    category = "Valuation",
    help_topic = "https://docs.example.invalid/math/scale",
    thread_safe
)]
fn calculate_area(width: f64, height: f64) -> f64 {
    width * height
}
```

| Option | Meaning | Default |
|---|---|---|
| `name = "..."` | Excel-visible function name | Rust function identifier |
| `id = "..."` | Stable UDF identity and generated export identity | Rust function identifier |
| `category = "..."` | Function Wizard category | add-in default when omitted or empty |
| `description = "..."` | Function Wizard description | joined Rust doc comments |
| `help_topic = "..."` | Help URL or topic supplied to Excel | empty |
| `thread_safe` | Register for Excel multi-threaded recalculation | disabled |
| `macro_sheet` | Register with macro-sheet capability | disabled |
| `volatile` | Recalculate whenever Excel recalculates | disabled |
| `hidden` | Hide the function from the Function Wizard | visible |

Use a stable explicit `id` before publishing workbooks. Changing an ID changes framework identity even when the Excel-visible name remains unchanged.

The macro supports at most 255 Excel-visible parameters for synchronous functions and 254 for async functions; Excel's async handle consumes the remaining ABI slot. The optional injected context is not Excel-visible.

### Function flag constraints

- `thread_safe` is incompatible with main-thread and macro-sheet contexts.
- `macro_sheet` is incompatible with `thread_safe` and async functions.
- reference arguments require macro-sheet capability.
- async functions cannot accept reference arguments.
- a return type must implement the marker trait for the selected execution mode.
- a volatile function's return type must also implement `VolatileReturn`.

See [Execution modes and contexts](execution-modes.md) and [Conversion reference](conversion-reference.md).

## `#[excel_context(...)]`

A function may have at most one injected context. It must be the first parameter, passed by value, and must not also carry `#[excel_arg]`.

```rust
fn lookup(
    #[excel_context(thread_safe)] context: ThreadSafeContext<'_, DeskTools>,
    key: String,
) -> XllResult<f64> {
    context.state().lookup(&key)
}
```

| Role | Rust context | Capability |
|---|---|---|
| `main_thread` | `MainThreadContext<'_, DeskTools>` | main-thread Excel callbacks, handles, RTD |
| `thread_safe` | `ThreadSafeContext<'_, DeskTools>` | shared state during MTR; no unsafe Excel callbacks |
| `macro_sheet` | `MacroSheetContext<'_, DeskTools>` | Excel references and macro-sheet registration |
| `asynchronous` | `AsyncContext<'_, DeskTools>` | cancellation and shared state for an async UDF |

An `async fn` may omit a context. When it has one, the role must be `asynchronous`. A synchronous function cannot use the asynchronous role.

## `#[excel_arg(...)]`

Annotate an Excel-visible parameter to improve Function Wizard metadata or define presence/reference policy:

```rust
fn interpolate(
    #[excel_arg(
        name = "Method",
        description = "Interpolation method.",
        default = Method::Linear,
        missing = "default",
        blank = "error"
    )]
    method: Method,
) -> f64 {
    // ...
}
```

| Option | Meaning |
|---|---|
| `name = "..."` | Excel-visible argument name |
| `description = "..."` | Function Wizard argument description |
| `default = <expr>` | Rust expression used by a selected default policy |
| `blank = "default"` | use `default` for an empty cell |
| `blank = "error"` | reject an empty cell explicitly |
| `missing = "default"` | use `default` for an omitted trailing argument |
| `missing = "error"` | reject an omitted argument explicitly |
| `reference` | receive an unevaluated Excel reference |

Rules:

- `default` requires at least one `blank = "default"` or `missing = "default"` policy.
- a `"default"` policy requires `default = ...`.
- `reference` cannot be combined with blank, missing, or default policies.
- `reference` requires `macro_sheet` or `MacroSheetContext`.
- argument patterns must be simple identifiers; destructuring belongs inside the function body.

Without an explicit presence policy, the parameter's conversion type controls blank and missing behavior. See [Optional arguments and enums](optional-arguments.md).

## `#[derive(ExcelEnum)]`

Derive strict text conversion for a fieldless enum:

```rust
{{#include ../../crates/xlfn/tests/ui/pass/excel_enum.rs:17:24}}
```

- variants must be unit variants;
- names default to Rust variant identifiers;
- `#[excel_value(name = "...")]` assigns the worksheet spelling;
- `#[excel_enum(ascii_case_insensitive)]` enables ASCII case-insensitive matching;
- effective names must be non-empty and unique under the selected comparison policy.

The derive implements input conversion, scalar output conversion, and all execution-mode return markers.

## `#[derive(ExcelHandleObject)]`

Derive this marker for an object that Excel formulas may own through an opaque typed handle:

```rust
#[derive(ExcelHandleObject)]
struct Dataset {
    // immutable or internally synchronized state
}
```

The type must satisfy `Any + Send + Sync + 'static`. Returning the object
publishes it for the producer formula's revision; a changed formula revision
publishes a new object while a same-revision recalculation reuses the memoized
object. Accepting `Handle<'_, Dataset>` resolves and type-checks the token.
`HandleAlias<'_, Dataset>` is the explicit main-thread return capability for
republishing an existing object. Borrowed `Handle` values are not return values,
and cannot be used by thread-safe, macro-sheet, or async functions. Async
functions that need an existing object use `HandleLease<Dataset>`, which is an
owned input created by leasing the authenticated registry object.

See [Formula-owned handles](handles.md).

## Treat compile errors as contract failures

The macros deliberately reject ambiguous or unsound declarations. Do not work around a diagnostic by weakening flags or changing an argument to a dynamic type without understanding the Excel ABI consequence. Compile-fail tests are appropriate for your own macro policies and published examples.
