# Function metadata

`#[excel_function]` is the single source of truth for registration metadata:

```rust
/// Computes the area of a rectangle.
#[excel_function(
    name = "MATH.AREA",
    id = "math_area",
    category = "Math",
    help_topic = "https://example.invalid/math/area",
    thread_safe
)]
fn calculate_area(
    #[excel_arg(name = "Width", description = "Rectangle width.")] width: f64,
    #[excel_arg(name = "Height", description = "Rectangle height.")] height: f64,
) -> f64 {
    width * height
}
```

## Function fields

| Field or flag | Meaning |
|---|---|
| `name = "..."` | Excel-visible function name; defaults to the Rust function name |
| `id = "..."` | stable generated export ID; defaults to the Rust function name |
| `category = "..."` | Function Wizard category; empty means the add-in default category |
| `description = "..."` | function description; defaults to joined Rust doc comments |
| `help_topic = "..."` | optional help URL or topic |
| `thread_safe` | registers the function for multi-threaded recalculation |
| `macro_sheet` | registers macro-sheet capability |
| `volatile` | asks Excel to recalculate the function as volatile |
| `hidden` | hides the function from normal Function Wizard discovery |

The `id` must be a Rust identifier fragment: ASCII letters, digits, and underscores, not beginning with a digit. It becomes part of an exported symbol and should remain stable once released.

Excel-visible function names use one ASCII case-insensitive identity rule for
duplicate validation and cleanup. Unicode case folding is not applied; if a
project uses non-ASCII names, their exact non-ASCII code points remain distinct.

## Argument metadata

```rust
#[excel_arg(
    name = "Factor",
    description = "Scaling factor for calculation."
)]
factor: f64
```

Argument names must be non-empty counted strings without comma, NUL, carriage return, or line feed. Each name and the joined comma-separated list must fit Excel's 32,767 UTF-16-unit counted-string limit.

Excel function registration has additional practical limits for argument help. xlfn supplies actual argument help for the leading entries and uses a terminal empty sentinel to avoid Excel's trailing-help truncation behavior. Treat concise argument names and descriptions as part of the stable worksheet API.

## Add-in metadata

At crate root:

```rust
#[excel_addin(
    name = "Math Analytics",
    id = "math-analytics",
    category = "Math"
)]
pub struct RatesAnalytics;
```

- `name` and `category` contain 1 to 255 UTF-16 code units;
- `id` is a non-reserved ASCII slug of at most 64 bytes;
- the ID begins with a letter and contains only letters, digits, `-`, or `_`;
- Windows reserved device names are rejected.

The ID participates in runtime identity, diagnostics, and temporary RTD registration ownership. Changing it is not a cosmetic release change.

## Visibility and naming policy

Prefer a project or organization prefix. Excel has a process-wide function namespace, and registration conflicts are rejected rather than overwritten. Avoid generic names such as `EVAL`, `VERSION`, or `LOOKUP` in distributed add-ins.
