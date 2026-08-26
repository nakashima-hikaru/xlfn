//! Recovery of host registration metadata left by an uncertain Excel call.

use crate::XllResult;
use crate::host_callback::HostCallbackSession;
use crate::registration::HostRegistrar;
use crate::runtime_components::HostLedger;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Retries metadata cleanup without making the lifecycle domain aware of
/// registration policy or host-side recovery details.
pub(crate) fn retry_metadata_debt(
    ledger: &HostLedger,
    callbacks: &mut HostCallbackSession,
) -> XllResult<()> {
    let debts = ledger.metadata_debt_snapshot();
    if debts.is_empty() {
        return Ok(());
    }

    let outcome = HostRegistrar::retry_metadata_debt(callbacks, &debts);
    ledger.replace_metadata_debt(outcome.remaining);
    for error in outcome.cleanup_issues {
        crate::boundary::report_cleanup_issue(&crate::shutdown::CleanupIssue {
            component: "Excel metadata debt result",
            kind: crate::shutdown::CleanupIssueKind::HostMemoryLeak,
            error,
        });
    }
    if let Some(error) = outcome.terminal {
        crate::boundary::report_boundary_error("xlAutoOpen metadata debt retry", &error);
        return Err(error);
    }
    if ledger.has_metadata_debt() {
        let count = ledger.metadata_debt_snapshot().len();
        let _ = catch_unwind(AssertUnwindSafe(|| {
            tracing::warn!(count, "Excel metadata debt remains after retry");
        }));
    }
    Ok(())
}
