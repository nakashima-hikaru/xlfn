# Create your first add-in

Build a worksheet function and call `=HELLO.ADD(2, 3)` in Excel.

This tutorial uses 64-bit Excel (`x86_64-pc-windows-msvc`). For 32-bit Excel, use
`i686-pc-windows-msvc` and the `package/win-x86/` output directory.

## 1. Create the project

Create a new Rust library project:

```powershell
cargo new --lib hello-xlfn
cd hello-xlfn
```

Update `Cargo.toml`:

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

The `crate-type = ["cdylib"]` setting instructs Cargo to produce a dynamic library
that `cargo-xlfn` packages into an `.xll`.

## 2. Define the add-in and function

Replace `src/lib.rs` with:

```rust
use xlfn::prelude::*;

#[excel_addin(name = "Hello Xll", id = "hello-xlfn", category = "Hello")]
pub struct HelloXll;

impl Addin for HelloXll {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> XllResult<Opened<(), (), ()>> {
        Ok(Opened::new(()))
    }
}

/// Adds two numbers.
#[excel_function(name = "HELLO.ADD", thread_safe)]
pub fn add(left: f64, right: f64) -> f64 {
    left + right
}
# fn main() {}
```

- `HelloXll` defines the add-in entry point and its metadata.
- `#[excel_function]` exposes the `add` function as `=HELLO.ADD` in Excel.
- The `thread_safe` flag enables concurrent evaluation on Excel's worker threads.

## 3. Build the XLL

Build the package:

```powershell
cargo xlfn package --target x86_64-pc-windows-msvc
```

This compiles the library and packages the XLL into:

```text
package/win-x64/hello-xlfn.xll
```

## 4. Call it in Excel

1. In Excel, go to **File → Options → Add-ins**.
2. Set **Manage** to **Excel Add-ins** and click **Go...**.
3. Click **Browse...** and select `package/win-x64/hello-xlfn.xll`.
4. Enter this formula in any cell:

```text
=HELLO.ADD(2, 3)
```

The cell displays **5**.

Next, explore [Worksheet functions](worksheet-functions.md) for more input types
and error handling.
