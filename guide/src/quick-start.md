# Create your first add-in

## 1. Install the project tool

Install `cargo-xlfn` from crates.io and add the MSVC compilation targets:

```powershell
cargo install cargo-xlfn --locked
rustup target add i686-pc-windows-msvc x86_64-pc-windows-msvc
```

From a local checkout, the equivalent command is:

```powershell
cargo install --path crates/cargo-xlfn --locked --force
```

Create a project outside the `xlfn` workspace checkout:

```powershell
cargo xlfn new hello-xlfn
cd hello-xlfn
```

Use `--bundle` when the application needs packaged sidecar files:

```powershell
cargo xlfn new data-xlfn --bundle
```

## 2. Understand the generated crate

The scaffold creates a `cdylib`, a single add-in definition in `src/lib.rs`, and worksheet functions in `src/udf.rs`. Its `Cargo.toml` contains an explicit distribution artifact name:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
xlfn = "0.1"

[package.metadata.xlfn]
artifact-name = "HelloXll"
```

The scaffold uses the release version embedded in `cargo-xlfn`. For a sibling local checkout during development, use a path dependency instead:

```toml
[dependencies]
xlfn = { path = "../xlfn/crates/xlfn" }
```

The crate-root add-in owns state and lifecycle:

```rust
#![deny(unsafe_op_in_unsafe_fn)]

use xlfn::prelude::*;

mod udf;

pub struct State;

#[excel_addin(
    name = "Hello Xll",
    id = "hello-xlfn",
    category = "HelloXll"
)]
pub struct HelloXll;

impl Addin for HelloXll {
    type State = State;
    type Error = XllError;

    fn open(context: &OpenContext) -> Result<State, XllError> {
        xlfn::diagnostics::install_file_diagnostic_sink(&context.build_info().addin_id)
            .map_err(|_| XllError::Internal {
                diagnostic_id: 0x4449_4147_5349_4e4b,
            })?;
        Ok(State)
    }
}
```

## 3. Add a function

In `src/udf.rs`:

```rust
use xlfn::prelude::*;

/// Adds two finite numbers.
#[excel_function(
    name = "HELLO.ADD",
    category = "Hello",
    help_topic = "https://example.invalid/hello/add",
    thread_safe
)]
pub fn add(
    #[excel_arg(name = "Left", description = "First addend.")] left: f64,
    #[excel_arg(name = "Right", description = "Second addend.")] right: f64,
) -> f64 {
    left + right
}
```

The doc comment becomes the function description unless `description = "..."` is supplied explicitly.

## 4. Validate linked artifacts

```powershell
cargo xlfn check
```

Without `--target`, `check` builds and validates both Windows targets. During development, validate one target explicitly:

```powershell
cargo xlfn check --target x86_64-pc-windows-msvc
```

This is stronger than `cargo check`: it links the DLL, stages an XLL package, verifies the `.xllexp` manifest, compares required exports with the PE export table, checks architecture, and resolves packaged imports.

## 5. Create a distribution

```powershell
cargo xlfn dist --all
```

The x86 and x64 packages are staged and validated before the output root is replaced. A failure in either target leaves the previous distribution in place.

For one target:

```powershell
cargo xlfn dist --target x86_64-pc-windows-msvc
```

## 6. Load the XLL in Excel

In Excel, open:

**File → Options → Add-ins → Manage: Excel Add-ins → Go → Browse**

Select the `.xll` in the directory matching the Excel process bitness. Keep the complete distribution directory together, including every packaged sidecar and `build-manifest.json`.

Enter:

```text
=HELLO.ADD(2, 3)
```

The result should be `5`. If loading or invocation fails, inspect the diagnostic log and follow [Troubleshooting](troubleshooting.md).
