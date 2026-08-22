# Create your first add-in

## 1. Setup compilation targets and optional tooling

Add the MSVC target for your target Excel bitness:

```powershell
rustup target add i686-pc-windows-msvc x86_64-pc-windows-msvc
```

`xlfn` is a library and framework crate. To perform automated PE artifact inspection and directory packaging, install the optional `cargo-xlfn` CLI tool:

```powershell
cargo install cargo-xlfn --locked
```

From a local checkout, the equivalent command is:

```powershell
cargo install --path crates/cargo-xlfn --locked --force
```

## 2. Create a library crate

Create a standard Rust library project:

```powershell
cargo new --lib hello-xlfn
cd hello-xlfn
```

In `Cargo.toml`, set the crate type to `cdylib` and add `xlfn` as a dependency:

```toml
[package]
name = "hello-xlfn"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
xlfn = "0.2"
```

For a local checkout during development, use a path dependency instead:

```toml
[dependencies]
xlfn = { path = "../xlfn/crates/xlfn" }
```

Define the add-in in `src/lib.rs`:

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
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(context: &OpenContext) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        context
            .diagnostics()
            .install_file_sink()
            .map_err(|error| XllError::Native {
                code: -1,
                message: error.to_string(),
            })?;
        Ok(Opened::new(State, (), ()))
    }
}
```

## 3. Add a function

Create `src/udf.rs`:

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

## 4. Validate linked artifacts (Optional)

`cargo-xlfn` provides artifact validation for Excel XLLs beyond standard `cargo check`:

```powershell
cargo xlfn check
```

Without `--target`, `check` builds and validates both Windows targets. During development, validate one target explicitly:

```powershell
cargo xlfn check --target x86_64-pc-windows-msvc
```

This links the DLL, stages an XLL package, verifies the `.xllexp` manifest, compares required exports with the PE export table, checks architecture, and resolves packaged imports.

## 5. Create a package

```powershell
cargo xlfn package --all
```

The x86 and x64 packages are staged and validated before the output root is replaced. A failure in either target leaves the previous package directory in place.

For one target:

```powershell
cargo xlfn package --target x86_64-pc-windows-msvc
```

*(Note: You can also build directly using `cargo build --target x86_64-pc-windows-msvc`, but `cargo xlfn package` provides automated XLL PE validation, sidecar handling, and transactional output staging.)*

## 6. Load the XLL in Excel

In Excel, open:

**File → Options → Add-ins → Manage: Excel Add-ins → Go → Browse**

Select the `.xll` in the directory matching the Excel process bitness (`package/win-x64` or `package/win-x86`). Keep the complete package directory together, including every packaged sidecar and `build-manifest.json`.

Enter:

```text
=HELLO.ADD(2, 3)
```

The result should be `5`. If loading or invocation fails, inspect the diagnostic log and follow [Troubleshooting](troubleshooting.md).
