// module: runtime
fn no_raw_release(runtime: &super::Runtime<()>) {
    let _ = runtime.release_module_residency(); // error: E0599
}
