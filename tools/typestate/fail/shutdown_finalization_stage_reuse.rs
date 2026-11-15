// module: runtime/shutdown/finalization
use super::{ArmedFinalization, BindingReleased, CompletionPendingBinding, FinalizationFailure, LogicalCompleted, TerminalCertificateKind};
use crate::Addin;

fn arm_twice<A: Addin, K: TerminalCertificateKind>(stage: CompletionPendingBinding<'_, A, K>) {
    let _armed = stage.arm_finalization();
    let _ = stage.arm_finalization(); // error: E0382
}

fn finish_logical_twice<A: Addin, K: TerminalCertificateKind>(stage: ArmedFinalization<'_, A, K>) {
    let _ = stage.finish_logical();
    let _ = stage.finish_logical(); // error: E0382
}

fn release_twice<A: Addin, K: TerminalCertificateKind>(stage: LogicalCompleted<'_, A, K>) {
    let _ = stage.release_binding();
    let _ = stage.release_binding(); // error: E0382
}

fn finalize_twice<A: Addin, K: TerminalCertificateKind>(stage: BindingReleased<'_, A, K>) {
    let _ = stage.finalize();
    let _ = stage.finalize(); // error: E0382
}

fn finalize_before_release<A: Addin, K: TerminalCertificateKind>(stage: LogicalCompleted<'_, A, K>) {
    let _ = stage.finalize(); // error: E0599
}

fn retry_after_failure<A: Addin, K: TerminalCertificateKind>(failure: FinalizationFailure<'_, A, K>) {
    let _ = failure.finish_logical(); // error: E0599
}
