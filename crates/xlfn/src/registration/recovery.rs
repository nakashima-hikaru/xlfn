//! Recovery of host registration metadata left by an uncertain Excel call.

use super::host::{RegistrationHost, RegistrationMutation};
use super::{ExcelNameKey, MetadataDebt, MetadataDebtRetryResult};
use crate::XllResult;
use crate::host_callback::HostCallbackSession;
use crate::runtime_components::HostLedger;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Retries metadata cleanup without making the lifecycle domain aware of
/// registration policy or host-side recovery details.
pub(crate) fn retry_metadata_debt(
    ledger: &HostLedger,
    callbacks: &HostCallbackSession,
) -> XllResult<()> {
    let debts = ledger.metadata_debt_snapshot();
    if debts.is_empty() {
        return Ok(());
    }

    let host = RegistrationHost::new(callbacks);
    let outcome = retry_metadata_debt_with_host(&host, &debts);
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

fn retry_metadata_debt_with_host(
    host: &RegistrationHost<'_>,
    debts: &BTreeMap<ExcelNameKey, Vec<MetadataDebt>>,
) -> MetadataDebtRetryResult {
    let mut remaining = BTreeMap::new();
    let mut cleanup_issues = Vec::new();
    let mut terminal = None;

    for (key, debt_bucket) in debts {
        if debt_bucket.is_empty() {
            continue;
        }
        if !host.permits_callbacks() {
            if let Some(status) = host.terminal_status() {
                terminal = Some(crate::XllError::ExcelApi {
                    function: crate::error::ExcelApiFunction::Evaluate,
                    failure: crate::error::ExcelApiFailure::Suppressed(status),
                });
            }
            remaining.insert(key.clone(), debt_bucket.clone());
            remaining.extend(
                debts
                    .range((std::ops::Bound::Excluded(key), std::ops::Bound::Unbounded))
                    .map(|(later_key, later_debt)| (later_key.clone(), later_debt.clone())),
            );
            break;
        }

        let probe = &debt_bucket[0];
        let current_registration = match host.metadata_debt_binding(probe.registration.excel_name) {
            Ok(registration) => registration,
            Err(error) => {
                remaining.insert(
                    key.clone(),
                    debt_bucket
                        .iter()
                        .map(|debt| debt.retry_failed(error.clone()))
                        .collect(),
                );
                if !host.permits_callbacks() {
                    terminal = Some(error);
                    remaining.extend(
                        debts
                            .range((std::ops::Bound::Excluded(key), std::ops::Bound::Unbounded))
                            .map(|(later_key, later_debt)| (later_key.clone(), later_debt.clone())),
                    );
                    break;
                }
                continue;
            }
        };

        let Some(current_registration) = current_registration else {
            // The name is already absent. The cleanup obligation is satisfied
            // without issuing a destructive call.
            continue;
        };

        let Some(matched_debt) = debt_bucket
            .iter()
            .find(|debt| debt.registration.id == current_registration)
        else {
            let error = crate::XllError::MetadataDebtBindingChanged {
                name: probe.registration.excel_name,
            };
            remaining.insert(
                key.clone(),
                debt_bucket
                    .iter()
                    .map(|debt| debt.retry_failed(error.clone()))
                    .collect(),
            );
            continue;
        };

        let mutation = host.delete_name(matched_debt.registration.excel_name);
        match mutation {
            RegistrationMutation::Applied { cleanup, .. } => {
                if let Err(error) = cleanup {
                    cleanup_issues.push(error);
                }
            }
            RegistrationMutation::Rejected { error } => {
                remaining.insert(
                    key.clone(),
                    debt_bucket
                        .iter()
                        .map(|debt| debt.retry_failed(error.clone()))
                        .collect(),
                );
            }
            RegistrationMutation::Indeterminate { status, error } => {
                remaining.insert(
                    key.clone(),
                    debt_bucket
                        .iter()
                        .map(|debt| debt.retry_failed(error.clone()))
                        .collect(),
                );
                if status.is_terminal() {
                    remaining.extend(
                        debts
                            .range((std::ops::Bound::Excluded(key), std::ops::Bound::Unbounded))
                            .map(|(later_key, later_debt)| (later_key.clone(), later_debt.clone())),
                    );
                    terminal = Some(error);
                    break;
                }
            }
        }
    }

    MetadataDebtRetryResult {
        remaining,
        cleanup_issues,
        terminal,
    }
}
