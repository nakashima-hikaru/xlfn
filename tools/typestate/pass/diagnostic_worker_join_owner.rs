// module: diagnostics/worker
fn ordered(worker: super::lifetime::DiagnosticWorker) -> bool {
    let joined = worker.join();
    let _ = joined.observer();
    joined.panicked()
}
