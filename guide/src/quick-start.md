# Create your first add-in

Build one worksheet function and call `=HELLO.ADD(2, 3)` in Excel. This tutorial
assumes the Windows tools and source checkout from [Requirements](requirements.md)
are ready. It uses 64-bit Excel; for 32-bit Excel, use the target and package
directory listed there.

## 1. Create the project beside the checkout

Start in the directory containing your `xlfn` checkout. Install the packaging
tool from that checkout, then create your add-in:

```powershell
cargo +1.98.1 install --path xlfn/crates/cargo-xlfn --locked
cargo new --lib hello-xlfn
cd hello-xlfn
rustup override set 1.98.1
```

The directories should now look like this:

```text
your-projects/
├── xlfn/
│   └── crates/
│       ├── cargo-xlfn/
│       └── xlfn/
└── hello-xlfn/        # run the remaining commands here
    ├── Cargo.toml
    └── src/lib.rs
```

Replace `hello-xlfn/Cargo.toml` with:

```toml
[package]
name = "hello-xlfn"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
xlfn = { path = "../xlfn/crates/xlfn" }
```

`cdylib` tells Cargo to produce the library that will become the XLL.

## 2. Add the function

Replace `src/lib.rs` with this complete example:

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
```

`HelloXll` defines the add-in. `Opened::new(())` opens it without application
state. `#[excel_function]` exposes `add` under the Excel name `HELLO.ADD`.
The `thread_safe` flag lets Excel call this pure calculation concurrently.

## 3. Build the XLL

From `hello-xlfn`, run:

```powershell
cargo xlfn package --target x86_64-pc-windows-msvc
```

This builds a release library, checks the linked artifact, and creates:

```text
package/win-x64/hello-xlfn.xll
```

Keep the complete `package/win-x64` directory together when moving the add-in.
It also contains package metadata and any required companion files.

## 4. Call it in Excel

In Excel, open **File → Options → Add-ins**. Select **Excel Add-ins** in the
**Manage** list, choose **Go → Browse**, and select
`hello-xlfn/package/win-x64/hello-xlfn.xll`.

Enter this formula in a cell:

```text
=HELLO.ADD(2, 3)
```

The result should be **5**. If your Excel uses semicolons to separate arguments,
enter `=HELLO.ADD(2; 3)` instead. For build, loading, or formula errors, see
[Troubleshooting](troubleshooting.md).

Next, add inputs and error handling in [Worksheet functions](worksheet-functions.md).
You can move functions into ordinary Rust modules as the project grows;
the macros register them without a separate function table.
