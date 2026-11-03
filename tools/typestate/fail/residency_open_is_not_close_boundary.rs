// module: runtime
fn open_is_not_close(
    runtime: &super::Runtime<()>,
    open: &crate::boundary::host::HostOpenOutcome,
) {
    let _ = runtime.release_closed_residency(open); // error: E0308
}
