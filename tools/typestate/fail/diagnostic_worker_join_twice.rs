// module: diagnostics/worker
fn invalid(worker: super::lifetime::DiagnosticWorker) {
    let _ = worker.join();
    let _ = worker.join(); // error: E0382
}
fn invalid_rejoin(worker: super::lifetime::JoinedDiagnosticWorker) {
    let _ = worker.join(); // error: E0599
}
