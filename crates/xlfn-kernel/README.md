# xlfn-kernel

Implementation-only ownership and concurrency primitives used by the [`xlfn`](https://crates.io/crates/xlfn) framework.

This crate has no Excel or COM dependencies. Its public items support the framework's internal adapters; they are not a supported direct application API. Add-ins should depend on `xlfn` instead.

`xlfn-kernel` is versioned independently and remains a pre-1.0 support crate when the application-facing `xlfn` API reaches 1.0. Its internal interfaces may change between minor versions.

Documentation: [User guide](https://nakashima-hikaru.github.io/xlfn/) | [API docs](https://docs.rs/xlfn-kernel) | [crates.io](https://crates.io/crates/xlfn-kernel)
