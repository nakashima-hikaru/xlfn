# Values and arrays

xlfn converts Excel values strictly. Ordinary parameters do not ask Excel to coerce text to numbers, booleans to numbers, or arrays to scalars. This makes worksheet behavior predictable and keeps conversion failures visible.

## Scalar inputs

| Rust type | Accepted Excel value | Notes |
|---|---|---|
| `f64` | number or integer | must be finite |
| `bool` | Boolean | numbers and text are not coerced |
| `i32` | integer, or an integral number in range | fractional values are rejected |
| `i64` | integer, or an exactly representable integral number | numeric input is limited to the exact Excel-double integer range, `-2^53..=2^53` |
| `String` | string | decoded as valid UTF-16 |
| `ExcelErrorValue` | Excel error | preserves the worksheet error |
| `ExcelSerialDate` | finite number | initially marked with `ExcelDateSystem::Workbook` |
| `ExcelValue` | supported scalar, error, blank, missing, or array | use for intentionally dynamic input |

An Excel error passed to a parameter that expects another type is propagated as that Excel error rather than disguised as a generic type error.

## Scalar outputs

The same scalar families can be returned. Important restrictions are:

- `f64` must be finite;
- `i64` must be exactly representable by an Excel number;
- strings must fit Excel's counted UTF-16 representation;
- `ExcelValue::Missing` and blank `ExcelCellValue` are input states, not valid worksheet results; use `ExcelErrorValue(ExcelError::NotAvailable)` for an explicit `#N/A` result;
- `()` is not a normal worksheet scalar result, although it is useful as an RTD value and in internal APIs.

Use `ExcelErrorValue(ExcelError::NotAvailable)` when an Excel error is the intended successful result. Use `Err(...)` when the function itself failed. That distinction improves diagnostics and instrumentation.

## Matrices

For synchronous functions that only inspect or transform an Excel array during the call, use `XlArrayRef<'_>`. It borrows the `xltypeMulti` cell buffer and converts each `XlValueRef` lazily, so input traversal itself allocates nothing:

```rust
#[excel_function(name = "ARRAY.SUM.BORROWED", thread_safe)]
fn sum_borrowed(values: XlArrayRef<'_>) -> XllResult<f64> {
    values
        .cells()
        .try_fold(0.0, |sum, cell| Ok(sum + cell.as_f64()?))
}
```

For large numeric outputs, explicitly import `xlfn::advanced::output::{XlArrayBuilder, XlArrayOutput}` and write directly into an `XlArrayBuilder`. The finished cell allocation is adopted by `ReturnBlock` without copying:

```rust
fn doubled(values: XlArrayRef<'_>) -> XllResult<XlArrayOutput> {
    let (rows, columns) = values.shape();
    let mut output = XlArrayBuilder::new(rows, columns)?;
    for cell in values.cells() {
        output.push_f64(cell.as_f64()? * 2.0)?;
    }
    output.finish()
}
```

Use the owned `Matrix<T>` path when values must outlive the exported call or cross into async work.

`Matrix<T>` stores a rectangular grid in row-major order:

```rust
#[excel_function(name = "ARRAY.IDENTITY", thread_safe)]
fn identity(size: i32) -> XllResult<Matrix<f64>> {
    let size = usize::try_from(size)
        .map_err(|_| XllError::input("size", InputError::OutOfRange))?;
    if size == 0 {
        return Err(XllError::input(
            "size",
            InputError::Malformed("matrix size must be non-zero"),
        ));
    }
    if size > 1_000 {
        return Err(XllError::input(
            "size",
            InputError::TooLarge {
                limit: 1_000,
                actual: size,
            },
        ));
    }
    let count = size.checked_mul(size).ok_or(XllError::Domain {
        code: DomainErrorCode::Overflow,
    })?;
    let mut values = vec![0.0; count];
    for index in 0..size {
        values[index * size + index] = 1.0;
    }
    Matrix::new(size, size, values)
}
```

`Matrix::new(rows, columns, data)` checks shape multiplication, Excel's row and column limits, framework element limits, and data length. A scalar input converts to a `1 x 1` matrix; an Excel multi-value converts to its rectangular shape.

Useful accessors include:

```rust
matrix.rows();
matrix.columns();
matrix.as_slice();
matrix.row(0);
matrix.column(0);
matrix.iter();
matrix[(0, 0)];
```

Indexing panics on an invalid coordinate. Use `row` and `column` when invalid coordinates should be handled as ordinary control flow.

## One-dimensional shapes

Use `Row<T>` and `Column<T>` to state orientation explicitly:

```rust
#[excel_function(name = "ARRAY.CUMSUM", thread_safe)]
fn cumulative(values: Row<f64>) -> XllResult<Row<f64>> {
    let mut total = 0.0;
    Row::new(
        values
            .into_vec()
            .into_iter()
            .map(|value| {
                total += value;
                total
            })
            .collect(),
    )
}
```

A `Row<T>` accepts a scalar or `1 x N` input. A `Column<T>` accepts a scalar or `N x 1` input. They reject a genuinely two-dimensional array instead of silently flattening it.

`Vec<T>` and `BoundedVarArgs<T, MAX>` are input-only one-dimensional containers. Prefer `BoundedVarArgs` for worksheet surfaces where a hard maximum is part of the contract:

```rust
#[excel_function(name = "STAT.MEAN", thread_safe)]
fn mean(values: BoundedVarArgs<f64, 128>) -> XllResult<f64> {
    let values = values.as_slice();
    if values.is_empty() {
        return Err(XllError::input("values", InputError::Malformed("empty input")));
    }
    Ok(values.iter().copied().sum::<f64>() / values.len() as f64)
}
```

`MAX` must be greater than zero.

## Array safety limits

The framework validates Excel's structural limits and imposes additional memory bounds before reading or allocating arrays.

| Limit | 32-bit target | 64-bit target |
|---|---:|---:|
| Excel rows | 1,048,576 | 1,048,576 |
| Excel columns | 16,384 | 16,384 |
| framework elements | 1,000,000 | 4,000,000 |
| referenced XLOPER12 bytes | 64 MiB | 256 MiB |
| returned allocation bytes | 64 MiB | 256 MiB |

The lower 32-bit limits are intentional. A 32-bit Excel process has a much smaller virtual address space, and one large array can destabilize the host even when the nominal worksheet dimensions are legal.

## Date serials

`ExcelSerialDate` preserves a finite Excel serial plus a date-system marker:

```rust
#[excel_function(name = "DATE.SERIAL", thread_safe)]
fn serial(date: ExcelSerialDate) -> f64 {
    date.serial()
}
```

An ordinary worksheet argument does not, by itself, reveal whether the workbook uses the Windows 1900 or Mac 1904 system, so converted inputs use `ExcelDateSystem::Workbook`. Resolve or inject the actual workbook convention in application policy before converting the serial to a civil date. `ExcelSerialDate::is_fictitious_1900_leap_day()` detects serial 60 only after the value has been marked `Windows1900`.

## Dynamic values

`ExcelValue` is useful for pass-through, inspection, and adapters whose type is intentionally dynamic. Prefer concrete Rust types in normal functions: they produce better Function Wizard signatures, clearer errors, and less downstream branching. Its array form contains only `ExcelCellValue`, so nested arrays and missing cells cannot be represented.
