# Test your add-in

Testing an xlfn add-in typically involves two levels: fast unit tests for
your calculation logic using standard `cargo test`, and artifact verification
using `cargo xlfn check` followed by validation in Excel.

## Unit test calculation logic

`#[excel_function]` converts Excel values to Rust types and delegates to your
function. To keep calculations easily testable without mock Excel environments,
keep the core computation in pure Rust functions:

```rust
{{#include ../fixtures/addin.md}}
# use xlfn::prelude::*;
pub fn compute_hypot(x: f64, y: f64) -> f64 {
    x.hypot(y)
}

#[excel_function(name = "MATH.HYPOT", thread_safe)]
pub fn hypot(x: f64, y: f64) -> f64 {
    compute_hypot(x, y)
}

#[cfg(test)]
mod tests {
    use super::compute_hypot;

    #[test]
    fn test_hypot() {
        assert_eq!(compute_hypot(3.0, 4.0), 5.0);
    }
}
```

Pure Rust functions can be tested on any platform with `cargo test`.
Because pure unit tests do not exercise Excel's argument conversion or C API
boundary, verify the integrated XLL artifact as well.

## Verify the XLL artifact with `cargo xlfn check`

`cargo xlfn check` verifies the built binary and bundle without starting Excel.
Run it for your target architectures:

```powershell
cargo xlfn check --target x86_64-pc-windows-msvc
cargo xlfn check --target i686-pc-windows-msvc
```

This checks:

- PE architecture matching the target (64-bit vs 32-bit);
- Required Excel exports and symbol decorations (including x86 stdcall);
- Embedded registration metadata;
- Packaged companion files and CRT dependency closure.

For more details on building and checking artifacts, see
[Build, validate, and load](build-validation.md).

## Test in Excel

Artifact checks verify binary structure, but end-to-end worksheet behavior
requires loading the add-in into Excel:

1. Build the package:
   ```powershell
   cargo xlfn package --target x86_64-pc-windows-msvc
   ```
2. In Excel, load the `.xll` via **File → Options → Add-ins → Excel Add-ins → Browse**.
3. Verify worksheet calls:
   - Call your functions with valid inputs and confirm expected outputs.
   - Test invalid inputs and boundary cases to confirm appropriate Excel error
     codes (`#VALUE!`, `#NUM!`, `#N/A`).
   - For stateful, async, or RTD functions, verify that recalculation, updates,
     and add-in unload behave cleanly.
