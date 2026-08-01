# C API integration overview

`xlfn` exposes Excel APIs only. It does not contain a DLL loader, worker pool, native
feature, or `OpenContext` loader extension.

Use clean layering between your native library and Excel:

```text
Native DLL / C API → Rust Wrapper / Domain SDK → XLL Add-in (xlfn)
```

The native wrapper owns raw FFI, safe domain types, RAII, and thread policy.
The XLL add-in owns Excel value conversion, UDF registration, handles, lifecycle, and error mapping.

## Loading from an XLL

Build the path directly from the host-reported module directory:

```rust
let library_path = context.module_directory().join("CalcEngine.dll");
// SAFETY: this final adapter owns the protected deployment policy and the
// supported native build's concurrent-call contract.
let engine = unsafe {
    DynamicEngine::load_trusted_concurrent(
        DynamicEngineConfig { library_path },
        ThreadPoolConfig { workers: 2 },
    )
}?;
```

Put the cloneable `Send + Sync` engine directly in shared add-in state and call
`DynamicEngine::close` from adapter shutdown. Its internal coordinator owns
thread-bound workers and the DLL root. Do not add engine-specific methods to
`OpenContext`.

## Deployment

List all sidecar files under `[package.metadata.xlfn.bundle]`. `cargo xlfn` validates paths,
duplicate basenames, PE architecture, DLL imports, and staging independently from runtime loading.
