// module: diagnostics
use super::worker::AsyncDiagnosticSink;
fn invalid(sink: &mut AsyncDiagnosticSink) {
    let _ = sink.sender.take(); // error: E0616
    let _ = sink.worker.take(); // error: E0616
}
