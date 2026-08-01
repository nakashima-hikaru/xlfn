# Optional arguments and enums

Excel distinguishes a missing positional argument from a reference to a blank cell. xlfn exposes that distinction instead of collapsing every absence into one value.

## `Option<T>`

For ordinary arguments, `Option<T>` maps both missing and blank to `None`:

```rust
#[excel_function(name = "DATA.TRANSFORM", thread_safe)]
fn scaled(value: f64, factor: Option<f64>) -> f64 {
    value / (1.0 + rate.unwrap_or(0.0))
}
```

Use this only when missing and blank have the same domain meaning.

## Preserve presence explicitly

`OptionalExcelValue<T>` has three variants:

```rust
pub enum OptionalExcelValue<T> {
    Missing,
    Blank,
    Value(T),
}
```

Example:

```rust
#[excel_function(name = "INPUT.STATE", thread_safe)]
fn state(value: OptionalExcelValue<String>) -> &'static str {
    match value {
        OptionalExcelValue::Missing => "missing",
        OptionalExcelValue::Blank => "blank",
        OptionalExcelValue::Value(_) => "value",
    }
}
```

This is the correct representation when omission means "use configuration" but a blank cell means "clear the setting", for example.

## Declarative blank and missing policies

`#[excel_arg]` can apply a policy before ordinary conversion:

```rust
#[excel_function(name = "RATE.COMPOUND", thread_safe)]
fn compound(
    principal: f64,
    #[excel_arg(
        name = "Rate",
        default = 0.0,
        missing = "default",
        blank = "error"
    )]
    rate: f64,
) -> f64 {
    principal * (1.0 + rate)
}
```

Allowed policy strings are `"default"` and `"error"`.

- `missing = "default"` evaluates the Rust `default` expression for a missing argument;
- `blank = "default"` evaluates it for a blank cell;
- `missing = "error"` rejects omission;
- `blank = "error"` rejects a blank cell;
- no policy means normal conversion applies.

A `default = ...` expression is accepted only when at least one presence state uses `"default"`. The expression is inserted into the generated Rust wrapper and must evaluate to the declared argument type.

Defaults should be deterministic and inexpensive. Do not hide I/O, locks, or mutable global state in a default expression.

Reference arguments cannot use blank, missing, or default policies because they preserve raw Excel reference semantics.

## Worksheet enums

Derive `ExcelEnum` for a small, closed string vocabulary:

```rust
#[derive(Clone, Copy, ExcelEnum)]
#[excel_enum(ascii_case_insensitive)]
enum Direction {
    #[excel_value(name = "Forward")]
    Forward,
    #[excel_value(name = "Reverse")]
    Reverse,
}

#[excel_function(name = "DIRECTION.SIGN", thread_safe)]
fn sign(direction: Direction) -> f64 {
    match direction {
        Direction::Forward => 1.0,
        Direction::Reverse => -1.0,
    }
}
```

The derive implements input conversion, output conversion, and all normal return-mode marker traits. Requirements:

- the target is an enum;
- every variant is unit-like;
- each Excel text value is non-empty;
- text values are unique under the selected comparison policy.

Without `ascii_case_insensitive`, matching is exact. With it, comparison is ASCII case-insensitive; it is not locale-sensitive Unicode case folding. The older `case_insensitive` spelling remains a compatibility alias.

Use `#[excel_value(name = "...")]` to make worksheet text independent of Rust naming. Once workbooks depend on those strings, treat them as versioned public API.

## Evolving an enum safely

Adding a new input value is usually backward-compatible. Renaming or removing one is not. A practical migration is:

1. retain the old variant text for at least one release;
2. map it to the new internal semantics;
3. emit a diagnostic or documentation deprecation notice;
4. remove it only in a planned breaking release.

The derive intentionally rejects aliases with duplicate text. When aliases are required during migration, implement `FromExcel` manually.
