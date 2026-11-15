// module: diagnostics/worker
fn invalid(worker: super::lifetime::DiagnosticWorker) {
    let _ = worker.running; // error: E0616
}
fn invalid_joined(joined: super::lifetime::JoinedDiagnosticWorker) {
    let _ = joined.observer; // error: E0616
}
