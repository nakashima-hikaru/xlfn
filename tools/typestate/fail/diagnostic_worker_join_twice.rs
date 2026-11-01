// module: diagnostics/worker
fn invalid(worker: super::lifetime::DiagnosticWorker) {
    let _ = worker.join();
    let _ = worker.join(); // error: E0382
}
