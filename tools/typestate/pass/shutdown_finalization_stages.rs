// module: runtime/shutdown/finalization
use super::{CompletionPendingBinding, FinalizationResult, Finalized, TerminalCertificateKind};
use crate::Addin;

fn finish<'runtime, A: Addin, K: TerminalCertificateKind>(
    certified: CompletionPendingBinding<'runtime, A, K>,
) -> FinalizationResult<'runtime, A, K, Finalized<'runtime, A, K>> {
    certified.arm_finalization().finish_logical()?.release_binding()?.finalize()
}
