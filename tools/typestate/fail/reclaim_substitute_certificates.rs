// module: runtime/shutdown/pipeline
use super::{ServicesQuiescent, TeardownTxn};
use crate::runtime::shutdown::FinalRemoval;

fn invalid<A: crate::Addin>(
    transaction: TeardownTxn<'_, A, FinalRemoval, ServicesQuiescent>,
    rtd: crate::excel_rtd_protocol::RtdQuiescent,
    diagnostics: crate::diagnostics::DiagnosticsStopped,
) {
    let _ = transaction.reclaim(|_| {}, rtd, diagnostics); // error: E0061
}
