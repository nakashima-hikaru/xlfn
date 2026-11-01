// module: diagnostics/worker
fn invalid(worker: super::lifetime::DiagnosticWorker) {
    let _ = worker.observer; // error: E0616
    let _ = worker.handle; // error: E0616
}
