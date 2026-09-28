# Values and arrays

Start with an owned `Matrix<T>` when a function accepts or returns a rectangular
range. Use borrowed views or incremental output when conversion cost matters.

xlfn converts Excel values strictly. Ordinary parameters do not ask Excel to
coerce text to numbers, booleans to numbers, or arrays to scalars.

## Choose a scalar type

| Rust input | Excel value | Notes |
| --- | --- | --- |
| `f64` | number or integer | must be finite |
| `bool` | Boolean | no numeric or text coercion |
| `i32` | integer or integral number | rejects fractions and overflow |
| `i64` | integer or exactly representable integral number | numeric input is limited to the exact binary64 integer range |
| `String` | string | owned UTF-8 after validated UTF-16 decoding |
| `&str` | string | call-local view for synchronous functions |
| `ExcelErrorValue` | Excel error | preserves the exact Excel error |
| `ExcelSerialDate` | finite number | retains an unresolved workbook date system |

An Excel error passed where a different type is expected is propagated as the
original error. Input views such as `ExcelCellRef` and `XlArrayRef` are
synchronous, call-scoped values; see [Borrow a range](#borrow-a-range-during-a-synchronous-call).

Built-in scalar outputs are `f64`, `bool`, `i32`, exactly representable `i64`,
`String`, `&str`, `ExcelSerialDate`, and `ExcelErrorValue`.
`ExcelCellOutput` and custom types implementing `IntoExcel` are also supported.
The dynamic `ExcelValue`, borrowed input views, and `()` are not ordinary
worksheet result types.

Numbers must be finite; strings must fit Excel's counted UTF-16 representation.
Return `ExcelErrorValue(ExcelError::NotAvailable)` for an intentional `#N/A`
value. Return `Err(...)` when the function failed: the latter is also reported
as a failure in diagnostics and instrumentation.

## Work with a rectangular range

Use an owned `Matrix<T>` for rectangular input or output. It can outlive an
exported call and is suitable when an async function needs owned input.

```rust
use xlfn::prelude::*;

#[excel_function(name = "ARRAY.SCALE", thread_safe)]
fn scale_grid(values: Matrix<f64>, factor: f64) -> XllResult<Matrix<f64>> {
    let (rows, columns) = (values.rows(), values.columns());
    let scaled = values.into_vec().into_iter().map(|value| value * factor).collect();
    Matrix::new(rows, columns, scaled)
}
```

With a two by two range in `A1:B2`, `=ARRAY.SCALE(A1:B2, 2)` returns a two by
two result. `Matrix::new` checks non-zero dimensions, element count, and
framework limits. A scalar input becomes a one by one matrix. Data is in
row-major order.

Read dimensions with `rows()` and `columns()`; use `as_slice()`, `row()`,
`column()`, and `iter()` for access. Indexing with `matrix[(row, column)]`
panics for an invalid coordinate. Use checked accessors for indices derived
from workbook input.

### Borrow a range during a synchronous call

`XlArrayRef<'_>` reads a mixed-value array without converting every cell
eagerly. Cell headers are validated on admission; payload conversion is lazy.

```rust
use xlfn::prelude::*;
use xlfn::value::XlArrayRef;

#[excel_function(name = "ARRAY.SUM.BORROWED", thread_safe)]
fn sum_borrowed(values: XlArrayRef<'_>) -> XllResult<f64> {
    values.cells().try_fold(0.0, |sum, cell| Ok(sum + cell.as_f64()?))
}
```

`MatrixRef<'_, T>` materializes `Copy` elements in call-local scratch. For
example, `MatrixRef<'_, &str>` avoids a separate owned string for each cell.
Neither borrowed view can escape the call or be an async argument.

### Build a large result incrementally

When an output is large or produced one cell at a time, `XlArrayBuilder`
writes the result without an intermediate owned matrix.

```rust
use xlfn::prelude::*;
use xlfn::output::{XlArrayBuilder, XlArrayOutput};
use xlfn::value::XlArrayRef;

#[excel_function(name = "ARRAY.DOUBLED", thread_safe)]
fn doubled(values: XlArrayRef<'_>) -> XllResult<XlArrayOutput> {
    let (rows, columns) = values.shape();
    let mut output = XlArrayBuilder::new(rows, columns)?;
    for cell in values.cells() {
        output.push_f64(cell.as_f64()? * 2.0)?;
    }
    output.finish()
}
```

The builder rejects non-finite output as `#NUM!` and checks dimensions and
return-storage limits.

## Keep one-dimensional shape explicit

`Row<T>` accepts a scalar or one row; `Column<T>` accepts a scalar or one
column. Both reject a genuinely two-dimensional array.

```rust
use xlfn::prelude::*;

#[excel_function(name = "ARRAY.CUMSUM", thread_safe)]
fn cumulative(values: Row<f64>) -> XllResult<Row<f64>> {
    let mut total = 0.0;
    Row::new(values.into_vec().into_iter().map(|value| {
        total += value;
        total
    }).collect())
}
```

`Vec<T>` and `BoundedVarArgs<T, MAX>` are input-only one-dimensional
containers. Use the bounded form when a maximum is part of the worksheet
contract; `MAX` must be greater than zero.

## Treat dates and dynamic values deliberately

`ExcelSerialDate` retains a finite Excel serial and an
`ExcelDateSystem::Workbook` marker. A cell alone does not say whether the
workbook uses the 1900 or 1904 date system. Resolve that convention in
application policy before converting to a civil date.

`ExcelValue` is an owned, intentionally dynamic **input** representation.
It does not implement the ordinary worksheet output contract. Convert it
deliberately to an output type instead of returning it directly. Prefer
concrete parameter types when possible: they give clearer errors and less
downstream branching. Its array form contains only `ExcelCellValue`, so
nested arrays and missing cells cannot be represented.

## Optional and omitted arguments

Wrap a parameter in `Option<T>` to accept empty or omitted arguments from Excel.
When the user leaves an argument blank or passes an empty cell, the parameter
receives `None`:

```rust
#[excel_function(name = "CALC.SCALE", thread_safe)]
fn scale(value: f64, factor: Option<f64>) -> f64 {
    value * factor.unwrap_or(1.0)
}
```

Next, see [Execution modes and contexts](execution-modes.md) to control how Excel runs your functions.
