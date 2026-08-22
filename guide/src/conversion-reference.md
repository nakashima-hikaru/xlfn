# Conversion reference

This chapter summarizes the built-in worksheet conversion surface. The behavioral chapters remain authoritative for design guidance; generated rustdoc remains authoritative for exact method signatures.

## Input conversions

| Rust parameter type | Accepted Excel representation | Important behavior |
|---|---|---|
| `f64` | number or integer | rejects non-finite values |
| `bool` | Boolean | no numeric or text coercion |
| `i32` | integer or integral number | rejects fractions and overflow |
| `i64` | integer or exactly representable integral number | numeric path is limited to the exact binary64 integer range |
| `String` | string | validates UTF-16 |
| `&str` | string | decodes UTF-16 into call scratch; synchronous UDFs only |
| `ExcelErrorValue` | Excel error | preserves the exact error |
| `ExcelCellRef<'call>` | number, Boolean, string, error, or blank | zero-allocation cell view; synchronous UDFs only |
| `ExcelSerialDate` | finite number | starts with `ExcelDateSystem::Workbook` |
| `Handle<'_, T>` | string handle token | authenticates, checks generation, and checks object type; valid only for the active call |
| `HandleLease<T>` | string handle token | authenticates and leases the typed object before an async future is scheduled; owned and call-independent |
| `Option<T>` | value, blank, or missing | blank and missing become `None` |
| `OptionalExcelValue<T>` | value, blank, or missing | preserves all three states |
| `XlArrayRef<'call>` | rectangular multi-value | zero-allocation borrowed cells; synchronous UDFs only |
| `Matrix<T>` | scalar or rectangular multi-value | scalar becomes `1 x 1`; validates shape and limits |
| `MatrixRef<'call, T>` | scalar or rectangular multi-value | call-scoped, call-scratch-materialized `Copy` element view; synchronous UDFs only |
| `Row<T>` | scalar or `1 x N` | rejects a true 2-D shape |
| `Column<T>` | scalar or `N x 1` | rejects a true 2-D shape |
| `Vec<T>` | scalar, row, or column | input only; rejects a true 2-D shape |
| `BoundedVarArgs<T, MAX>` | scalar, row, or column | input only; requires `MAX > 0` and enforces the bound |
| `ExcelValue` | supported scalar, error, blank, missing, or array | intentionally dynamic, owned input representation; array cells are `ExcelCellValue` |
| a type deriving `ExcelEnum` | string | exact or optional ASCII case-insensitive match |
| a custom `T: FromExcel<'call>` | defined by the implementation | may borrow only for the generated call lifetime |

An Excel error supplied where another type is expected is propagated as that Excel error. Ordinary conversions do not ask Excel to coerce strings, booleans, references, or arrays into unrelated types.

## Reference conversions

A parameter marked `#[excel_arg(reference)]` uses `FromExcelReference<'call>`, not `FromExcel`.

The built-in `ExcelReference<'call>` preserves:

- same-sheet or sheet-qualified identity;
- one or more rectangular areas;
- zero-based row and column bounds;
- a lifetime tied to the active Excel call.

Reference parameters require macro-sheet capability and are unavailable to async functions. They are raw call-scoped capabilities rather than ordinary formula-revision inputs. Copy only bounded metadata out of the borrowed value. Use the main-thread reference APIs to coerce or inspect cells when required.

## Scalar output conversions

The following are direct scalar returns:

- `f64`, `bool`, `i32`, and exactly representable `i64`;
- `String` and `&str`;
- `ExcelErrorValue`;
- `ExcelSerialDate`;
- `ExcelCellOutput` and custom `IntoExcel` implementations;
- a type deriving `ExcelEnum`;
- `RtdValue`.

`Matrix<T>`, `Row<T>`, and `Column<T>` are owned array returns when every element implements `IntoExcel`. With the explicit `unstable` crate feature, `xlfn::unstable::output::XlArrayBuilder::new` produces `XlArrayOutput` directly in the final `XLOPER12` cell buffer, avoiding an intermediate cell vector and a cell-buffer copy.

`Result<T, E>` is supported whenever `T` is supported for the selected execution mode and `E: IntoXllError`. The wrapper converts the error exactly once at the Excel boundary.

## Execution-mode return matrix

| Return family | Main thread | Thread-safe | Macro-sheet | Async | Volatile |
|---|:---:|:---:|:---:|:---:|:---:|
| built-in scalar | yes | yes | yes | yes | yes |
| `ExcelEnum` | yes | yes | yes | yes | yes |
| `Matrix<T>`, `Row<T>`, `Column<T>`, `XlArrayOutput` | yes | yes | yes | yes | yes |
| `RtdValue` | yes | yes | yes | yes | yes |
| `HandleAlias<'_, T>` | yes | no | no | no | yes |
| object deriving `ExcelHandleObject` | yes | no | no | no | yes |
| custom `T` | according to implemented marker traits | according to implemented marker traits | according to implemented marker traits | according to implemented marker traits | according to implemented marker traits |

“Volatile” is an additional marker, not an execution thread. A volatile thread-safe function, for example, needs both `ThreadSafeReturn` and `VolatileReturn`.

## Presence behavior

Excel distinguishes:

- **value**: a normal scalar, array, error, or reference;
- **blank**: an empty cell (`xltypeNil`);
- **missing**: an omitted trailing argument (`xltypeMissing`).

Choose among:

- `Option<T>` when blank and missing are intentionally equivalent;
- `OptionalExcelValue<T>` when the distinction matters;
- `#[excel_arg(blank = ..., missing = ..., default = ...)]` when presence policy belongs in the worksheet signature;
- a required `T` when neither state is valid.

See [Optional arguments and enums](optional-arguments.md).

## Arrays and allocation limits

`XlArrayRef` is the allocation-free mixed-value input path. It exposes shape, indexed access, and lazy cell iteration through `XlValueRef`; `XlStrRef` borrows a string's UTF-16 units until decoding is actually requested. Use `XlArrayRef` when lazy raw-cell access is enough. Use `&str`, `ExcelCellRef`, or `MatrixRef<T>` when a synchronous function needs typed call-local values; `MatrixRef` materializes its `Copy` elements in call scratch. Use `String`, `ExcelCellValue`, `Matrix<T>`, or `Vec<T>` when the input must be owned, especially for async work.

Borrowed strings and grids use one `CallScope` scratch root. String decoding allocates UTF-8 bytes there, and borrowed matrix elements are stored there only when `T: Copy`; no destructor-bearing collection is placed in call scratch. The scope is dropped after the generated synchronous call returns.

`Matrix::new` and `XlArrayBuilder::new` require non-zero dimensions, checked multiplication, matching element count, and values within both Excel and framework limits.

| Limit | x86 | x64 |
|---|---:|---:|
| worksheet rows | 1,048,576 | 1,048,576 |
| worksheet columns | 16,384 | 16,384 |
| framework array elements | 1,000,000 | 4,000,000 |
| referenced Excel allocation bytes | 64 MiB | 256 MiB |
| returned allocation bytes | 64 MiB | 256 MiB |

Validate application-specific limits before allocating. Framework checks are host-protection ceilings, not a recommendation to return multi-million-cell arrays routinely.

## Dynamic value variants

`ExcelCellValue` represents:

```text
Number | Boolean | String | Error | Blank
```

`ExcelValue` represents:

```text
Scalar(ExcelCellValue) | Missing | Array(Matrix<ExcelCellValue>)
```

`Missing` is an omitted argument and cannot occur inside an array. `Blank` is an empty cell and can occur as a scalar or array cell. The raw `xltypeInt` transport form is canonicalized to `Number`; it is not an input semantic variant. `ExcelCellOutput` intentionally has no blank or missing variant. Return `ExcelErrorValue(ExcelError::NotAvailable)` or an explicit `ExcelCellOutput::Error` when an unavailable result is intended.

## Custom conversion checklist

For `FromExcel<'call>` and the built-in borrowed parameter views:

1. inspect only the active `XlValueRef<'_>`;
2. copy owned data before returning;
3. use the supplied static argument name in `XllError::Input`;
4. reject unsupported coercions and non-finite values explicitly;
5. bound all allocation from workbook-controlled lengths.
6. keep owned conversion independent of framework runtime state; do not retain temporary Excel pointers or call-scoped views in an owned result.

For `IntoExcel`:

1. validate the application value before allocation;
2. do not call Excel from thread-safe or async conversion paths;
3. preserve ownership until Excel calls `xlAutoFree12` through framework-managed return storage.

See [Custom conversions](custom-conversions.md).
