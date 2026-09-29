//! Recovery of host registration metadata left by an uncertain Excel call.

use super::host::{RegistrationHost, RegistrationMutation};
use super::{ExcelNameKey, MetadataDebt, MetadataDebtRetryResult};
use crate::XllResult;
use crate::host_callback::HostCallbackSession;
use crate::panic_boundary::catch_no_unwind;
use crate::runtime_components::HostLedger;
use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;

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
        let _ = catch_no_unwind(AssertUnwindSafe(|| {
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

        let mutation = host.delete_name_if_binding_matches(
            debt_bucket[0].registration.excel_name,
            debt_bucket.iter().map(|debt| debt.registration.id),
        );
        match mutation {
            RegistrationMutation::Applied { cleanup, .. } => {
                if let Err(error) = cleanup {
                    cleanup_issues.push(error);
                }
            }
            RegistrationMutation::Rejected { error }
            | RegistrationMutation::Indeterminate { error, .. } => {
                remaining.insert(
                    key.clone(),
                    debt_bucket
                        .iter()
                        .map(|debt| debt.retry_failed(error.clone()))
                        .collect(),
                );
                if !host.permits_callbacks() {
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

#[cfg(all(
    test,
    any(not(target_os = "windows"), feature = "async", feature = "handles")
))]
mod tests {
    use super::*;
    use crate::XllError;
    use crate::registration::RegistrationId;
    use crate::registration::host::test_support::{CallbackScript, Reply};
    use crate::return_abi::ExcelCallbackStatus;
    use xlfn_sys::{XLF_EVALUATE, XLF_SET_NAME, XLOPER12, XLRET_ABORT, XLRET_FAILED};

    fn debt(name: &'static str, id: f64) -> MetadataDebt {
        MetadataDebt::new(
            RegistrationId {
                id,
                excel_name: name,
            },
            XllError::Closing,
        )
    }

    fn debts(
        entries: impl IntoIterator<Item = MetadataDebt>,
    ) -> BTreeMap<ExcelNameKey, Vec<MetadataDebt>> {
        let mut debts = BTreeMap::<_, Vec<_>>::new();
        for debt in entries {
            debts.entry(debt.key()).or_default().push(debt);
        }
        debts
    }

    #[test]
    fn retry_deletes_only_a_binding_owned_by_its_debt_bucket() {
        let script = CallbackScript::install([
            Reply::success(XLF_EVALUATE, XLOPER12::number(-9.0)),
            Reply::success(XLF_SET_NAME, XLOPER12::boolean(true)),
        ]);
        let callbacks = HostCallbackSession::new();
        let host = RegistrationHost::new(&callbacks);
        let debts = debts([debt("TEST.OWNED", 7.0), debt("TEST.OWNED", -9.0)]);

        let outcome = retry_metadata_debt_with_host(&host, &debts);

        assert!(outcome.remaining.is_empty());
        assert!(outcome.cleanup_issues.is_empty());
        assert!(outcome.terminal.is_none());
        script.assert_calls(&[XLF_EVALUATE, XLF_SET_NAME]);
    }

    #[test]
    fn retry_resolves_an_absent_name_without_deleting_it() {
        let script = CallbackScript::install([Reply::success(
            XLF_EVALUATE,
            XLOPER12::error(xlfn_sys::XLERR_NAME),
        )]);
        let callbacks = HostCallbackSession::new();
        let host = RegistrationHost::new(&callbacks);
        let debts = debts([debt("TEST.ABSENT", 7.0)]);

        let outcome = retry_metadata_debt_with_host(&host, &debts);

        assert!(outcome.remaining.is_empty());
        assert!(outcome.terminal.is_none());
        script.assert_calls(&[XLF_EVALUATE]);
    }

    #[test]
    fn retry_preserves_a_rebound_name_and_all_its_debts() {
        let script =
            CallbackScript::install([Reply::success(XLF_EVALUATE, XLOPER12::number(11.0))]);
        let callbacks = HostCallbackSession::new();
        let host = RegistrationHost::new(&callbacks);
        let debts = debts([debt("TEST.REBOUND", 7.0), debt("TEST.REBOUND", 9.0)]);

        let outcome = retry_metadata_debt_with_host(&host, &debts);

        let remaining = &outcome.remaining[&ExcelNameKey::new("TEST.REBOUND")];
        assert_eq!(remaining.len(), 2);
        for debt in remaining {
            assert!(matches!(
                debt.last_error(),
                XllError::MetadataDebtBindingChanged {
                    name: "TEST.REBOUND"
                }
            ));
        }
        assert!(outcome.terminal.is_none());
        script.assert_calls(&[XLF_EVALUATE]);
    }

    #[test]
    fn retry_preserves_current_and_later_debts_after_terminal_callbacks() {
        for deletion_started in [false, true] {
            let replies = if deletion_started {
                vec![
                    Reply::success(XLF_EVALUATE, XLOPER12::number(7.0)),
                    Reply::success(XLF_SET_NAME, XLOPER12::nil()).status(XLRET_ABORT),
                ]
            } else {
                vec![Reply::success(XLF_EVALUATE, XLOPER12::nil()).status(XLRET_ABORT)]
            };
            let script = CallbackScript::install(replies);
            let callbacks = HostCallbackSession::new();
            let host = RegistrationHost::new(&callbacks);
            let debts = debts([debt("A.OWNED", 7.0), debt("B.OWNED", 9.0)]);

            let outcome = retry_metadata_debt_with_host(&host, &debts);

            assert_eq!(outcome.remaining.len(), 2);
            assert!(outcome.terminal.is_some());
            assert!(!host.permits_callbacks());
            assert!(matches!(
                outcome.remaining[&ExcelNameKey::new("B.OWNED")][0].last_error(),
                XllError::Closing
            ));
            script.assert_calls(if deletion_started {
                &[XLF_EVALUATE, XLF_SET_NAME]
            } else {
                &[XLF_EVALUATE]
            });
        }
    }

    #[test]
    fn retry_respects_callback_suppression_without_touching_names() {
        let script = CallbackScript::install([]);
        let callbacks = HostCallbackSession::new();
        callbacks.suppress_for_test(ExcelCallbackStatus::Abort);
        let host = RegistrationHost::new(&callbacks);
        let debts = debts([debt("A.OWNED", 7.0), debt("B.OWNED", 9.0)]);

        let outcome = retry_metadata_debt_with_host(&host, &debts);

        assert_eq!(outcome.remaining.len(), 2);
        assert!(outcome.terminal.is_some());
        script.assert_calls(&[]);
    }

    #[test]
    fn retry_keeps_unreadable_debt_and_continues_after_nonterminal_errors() {
        let script = CallbackScript::install([
            Reply::success(XLF_EVALUATE, XLOPER12::nil()).status(XLRET_FAILED),
            Reply::success(XLF_EVALUATE, XLOPER12::error(xlfn_sys::XLERR_NAME)),
        ]);
        let callbacks = HostCallbackSession::new();
        let host = RegistrationHost::new(&callbacks);
        let debts = debts([debt("A.OWNED", 7.0), debt("B.OWNED", 9.0)]);

        let outcome = retry_metadata_debt_with_host(&host, &debts);

        assert_eq!(outcome.remaining.len(), 1);
        assert!(
            outcome
                .remaining
                .contains_key(&ExcelNameKey::new("A.OWNED"))
        );
        assert!(outcome.terminal.is_none());
        script.assert_calls(&[XLF_EVALUATE, XLF_EVALUATE]);
    }

    #[test]
    fn retry_commits_name_deletion_before_result_cleanup() {
        let script = CallbackScript::install([
            Reply::success(XLF_EVALUATE, XLOPER12::number(7.0)),
            Reply::success(XLF_SET_NAME, XLOPER12::boolean(true)).release_status(XLRET_FAILED),
        ]);
        let callbacks = HostCallbackSession::new();
        let host = RegistrationHost::new(&callbacks);
        let debts = debts([debt("TEST.OWNED", 7.0)]);

        let outcome = retry_metadata_debt_with_host(&host, &debts);

        assert!(outcome.remaining.is_empty());
        assert_eq!(outcome.cleanup_issues.len(), 1);
        assert!(outcome.terminal.is_none());
        script.assert_calls(&[XLF_EVALUATE, XLF_SET_NAME]);
    }
}
