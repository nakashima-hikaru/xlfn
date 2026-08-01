# Excel references

Ordinary xlfn arguments receive coerced values. Use a raw reference only when the function needs coordinates, sheet identity, multiple areas, or an explicit Excel callback such as coercing a range at call time.

## Declare a reference parameter

A raw reference must use `#[excel_arg(reference)]` and requires macro-sheet capability:

```rust
#[excel_function(name = "RANGE.AREA.COUNT")]
fn area_count(
    #[excel_context(macro_sheet)] _context: MacroSheetContext<'_, State>,
    #[excel_arg(reference, description = "A cell or range reference.")]
    reference: ExcelReference<'_>,
) -> XllResult<i32> {
    i32::try_from(reference.areas().count())
        .map_err(|_| XllError::Domain {
            code: xlfn::error::DomainErrorCode::Overflow,
        })
}
```

The function may instead use the `macro_sheet` flag when no context is needed, but a context is required to call `coerce` or `sheet_name`.

Reference arguments are incompatible with asynchronous functions and cannot use blank, missing, or default policies.

## Lifetime and thread restrictions

`ExcelReference<'call>` is a borrowed view over Excel-owned memory. It is deliberately neither `Send` nor `Sync`, and it is valid only for the current exported call.

Do not:

- store it in add-in state;
- place it in a handle object;
- move it to a worker thread;
- capture it in an async future;
- retain its raw pointer after the function returns.

Extract coordinates or coerce the data into an owned value first.

## Coordinates and areas

`ReferenceArea` coordinates are zero-based and inclusive:

```rust
for area in reference.areas() {
    let first_row = area.first_row();
    let last_row = area.last_row();
    let first_column = area.first_column();
    let last_column = area.last_column();
}
```

The framework validates Excel's worksheet bounds. A reference can contain at most 1,024 areas.

`reference.sheet_id()` returns:

- `None` for a same-sheet `SRef`, where Excel does not carry an explicit sheet ID;
- `Some(SheetId)` for a sheet-qualified `Ref`.

`is_multi_area()` reports whether a qualified reference contains more than one area.

## Coerce to owned data

Use `MacroSheetContext` before leaving the call:

```rust
#[excel_function(name = "RANGE.SUM")]
fn range_sum(
    #[excel_context(macro_sheet)] context: MacroSheetContext<'_, State>,
    #[excel_arg(reference)] reference: ExcelReference<'_>,
) -> XllResult<f64> {
    let values: Matrix<f64> = context.coerce_matrix(&reference)?;
    Ok(values.iter().copied().sum())
}
```

`coerce` returns `OwnedExcelValue`; `coerce_matrix<T>` applies normal element conversion to an owned matrix. Once coercion succeeds, the resulting value can be retained or passed to internal workers according to its Rust `Send`/`Sync` properties.

Coercion is an Excel callback and must occur on the permitted thread. It can fail because the reference is invalid, Excel rejects the callback in the current state, or an element cannot convert to `T`.

## Sheet names

`MacroSheetContext::sheet_name(&reference)` asks Excel for the referenced sheet name. Do not infer names from opaque `SheetId` values. Sheet IDs are process/workbook identifiers, not stable persisted keys.

## Prefer values unless coordinates matter

A raw reference changes recalculation and threading capabilities. It also makes testing more dependent on Excel. Use an ordinary `Matrix<T>` when the function only needs values. Reserve references for operations whose semantics genuinely depend on location or Excel reference identity.
