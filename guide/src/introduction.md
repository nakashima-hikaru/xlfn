# Excel functions, written in Rust

xlfn turns typed Rust functions into native Microsoft Excel XLL add-ins.
You write the calculation; xlfn handles registration and value conversion at the Excel boundary.

```rust
{{#include ../fixtures/addin.md}}
use xlfn::prelude::*;

#[excel_function(name = "HELLO.ADD", thread_safe)]
fn add(left: f64, right: f64) -> f64 {
    left + right
}
```

With an add-in definition, this becomes `=HELLO.ADD(2, 3)` in a worksheet.
The [quick start](quick-start.md) provides a complete working example.

## Getting started

To build and load an XLL, you need Windows and desktop Excel:

1. [Set up your environment](requirements.md): install build tools and the matching target.
2. [Create your first add-in](quick-start.md): build your first function and verify `=HELLO.ADD(2, 3)` in Excel.

## Guide overview

Explore specific topics as your add-in grows:

| Task | Chapter |
| --- | --- |
| Accept ranges or return grids | [Values and arrays](values.md) |
| Run calculations on Excel worker threads | [Execution modes](execution-modes.md) |
| Manage state, cache values, or stream updates | [Choose a calculation pattern](choosing-pattern.md) |
| Return Excel errors and collect logs | [Errors and diagnostics](errors-diagnostics.md) |
| Build and validate the XLL package | [Build and load](build-validation.md) |
| Test your add-in | [Test your add-in](testing.md) |
| Deploy to end users | [Deployment and distribution](deployment.md) |
| Resolve build, load, or formula issues | [Troubleshooting](troubleshooting.md) |

## API documentation and examples

- [API Reference (docs.rs)](https://docs.rs/xlfn) covers function signatures, types, and attributes.
- Working examples are available in the [examples directory](https://github.com/nakashima-hikaru/xlfn/tree/main/examples) of the repository.
