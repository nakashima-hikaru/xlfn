# Excel functions, written in Rust

xlfn turns typed Rust functions into native Microsoft Excel XLL add-ins.
You write the calculation; xlfn registers the function and converts its inputs
and results at the Excel boundary.

```rust
use xlfn::prelude::*;

#[excel_function(name = "HELLO.ADD", thread_safe)]
fn add(left: f64, right: f64) -> f64 {
    left + right
}
```

With an add-in definition, this becomes `=HELLO.ADD(2, 3)` in a worksheet.
The [first add-in tutorial](quick-start.md) provides the complete project.

## Start here

You need working knowledge of Rust and access to Windows and desktop Excel
to build and load an XLL. No knowledge of the Excel C API is needed for the
tutorial.

1. [Set up your environment](requirements.md): select the target matching your Excel installation.
2. [Create your first add-in](quick-start.md): build one function and see `5` in a cell.

## Find what you need

After the tutorial, open only the chapter that matches your next task:

| I want to… | Start with |
| --- | --- |
| Accept a range or return a table | [Values and arrays](values.md) |
| Run calculations on Excel's worker threads | [Execution modes](execution-modes.md) |
| Keep objects, cache results, or work in the background | [Choose a calculation pattern](choosing-pattern.md) |
| Return an error or collect diagnostic logs | [Errors and diagnostics](errors-diagnostics.md) |
| Package an add-in for other people | [Build and load](build-validation.md), then [distribution](deployment.md) |
| Fix a loading, formula, or update problem | [Troubleshooting](troubleshooting.md) |

For exact attribute options, conversions, and packaging settings, use the
[reference documents](https://github.com/nakashima-hikaru/xlfn/tree/main/docs/reference)
or the guide's search.

## Examples and API documentation

The repository contains a [basic add-in](https://github.com/nakashima-hikaru/xlfn/tree/main/examples/basic-xll)
and an [RTD source example](https://github.com/nakashima-hikaru/xlfn/tree/main/examples/rtd-source).
Use [rustdoc](https://docs.rs/xlfn) for API signatures.

xlfn is pre-1.0. See [compatibility](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/reference/compatibility.md) before upgrading and
[Validate in Excel](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/EXCEL_TESTING.md) before distributing an add-in.
