//! Recovery of host registration metadata left by an uncertain Excel call.

use super::host::{RegistrationHost, RegistrationMutation};
use super::{MetadataDebtRetryResult, MetadataDebts};
use crate::XllResult;
use crate::host_callback::HostCallbackSession;
use crate::panic_boundary::catch_no_unwind;
use crate::runtime_components::HostLedger;
use std::panic::AssertUnwindSafe;

/// Retries metadata cleanup without making the lifecycle domain aware of
/// registration policy or host-side recovery details.
pub(crate) fn retry_metadata_debt(
    ledger: &HostLedger,
    callbacks: &HostCallbackSession,
) -> XllResult<()> {
    let host = RegistrationHost::new(callbacks);
    if !ledger.metadata_debt_status().has_pending_work() {
        return terminal_retry_error(&host).map_or(Ok(()), Err);
    }

    let outcome = ledger.retry_metadata_cleanup(&host);
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
    let status = ledger.metadata_debt_status();
    if status.has_pending_work() {
        let _ = catch_no_unwind(AssertUnwindSafe(|| {
            tracing::warn!(
                count = status.pending_buckets,
                cleanup_in_progress = status.cleanup_in_progress,
                "Excel metadata debt remains after retry"
            );
        }));
    }
    Ok(())
}

pub(crate) fn retry_metadata_debt_with_host(
    host: &RegistrationHost<'_>,
    debts: MetadataDebts,
) -> MetadataDebtRetryResult {
    let mut debts = debts.into_iter();
    let mut remaining = MetadataDebts::new();
    let mut cleanup_issues = Vec::new();
    let mut terminal = None;

    while let Some((key, debt_bucket)) = debts.next() {
        if debt_bucket.is_empty() {
            continue;
        }
        if !host.permits_callbacks() {
            terminal = terminal_retry_error(host);
            remaining.insert(key, debt_bucket);
            remaining.extend(debts);
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
                    key,
                    debt_bucket
                        .into_iter()
                        .map(|debt| debt.with_retry_failure(error.clone()))
                        .collect(),
                );
                if !host.permits_callbacks() {
                    remaining.extend(debts);
                    terminal = Some(error);
                    break;
                }
            }
        }
    }

    // Successful deletion commits before releasing the callback result. Its
    // final xlFree may still stop this Excel entrypoint even when there is no
    // later debt whose admission check would observe that terminal status.
    terminal = terminal.or_else(|| terminal_retry_error(host));
    MetadataDebtRetryResult {
        remaining,
        cleanup_issues,
        terminal,
    }
}

fn terminal_retry_error(host: &RegistrationHost<'_>) -> Option<crate::XllError> {
    host.terminal_status().map(|status| {
        crate::error::construct::excel_api(
            crate::error::ExcelApiFunction::Evaluate,
            crate::error::ExcelApiFailure::Suppressed(status),
        )
    })
}

#[cfg(all(
    test,
    any(not(target_os = "windows"), feature = "async", feature = "handles")
))]
mod tests {
    use super::*;
    use crate::XllError;
    use crate::registration::host::test_support::{CallbackScript, Reply};
    use crate::registration::{ExcelNameKey, MetadataDebt, RegistrationId};
    use crate::return_abi::ExcelCallbackStatus;
    use xlfn_sys::{
        XLF_EVALUATE, XLF_SET_NAME, XLOPER12, XLRET_ABORT, XLRET_FAILED, XLRET_UNCALCED,
    };

    fn debt(name: &'static str, id: f64) -> MetadataDebt {
        MetadataDebt::new(
            RegistrationId {
                id,
                excel_name: name,
            },
            XllError::CLOSING,
        )
    }

    fn debts(entries: impl IntoIterator<Item = MetadataDebt>) -> MetadataDebts {
        let mut debts = MetadataDebts::new();
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

        let outcome = retry_metadata_debt_with_host(&host, debts);

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

        let outcome = retry_metadata_debt_with_host(&host, debts);

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

        let outcome = retry_metadata_debt_with_host(&host, debts);

        let remaining = &outcome.remaining[&ExcelNameKey::new("TEST.REBOUND")];
        assert_eq!(remaining.len(), 2);
        for debt in remaining {
            assert_eq!(debt.attempts(), 2);
            assert!(matches!(
                crate::error::construct::view(debt.last_error()),
                crate::error::construct::ErrorView::MetadataDebtBindingChanged {
                    name: "TEST.REBOUND",
                    ..
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
            let later = ExcelNameKey::new("B.OWNED");
            let later_bucket = debts[&later].as_ptr();

            let outcome = retry_metadata_debt_with_host(&host, debts);

            assert_eq!(outcome.remaining.len(), 2);
            assert!(outcome.terminal.is_some());
            assert!(!host.permits_callbacks());
            assert_eq!(
                outcome.remaining[&ExcelNameKey::new("A.OWNED")][0].attempts(),
                2
            );
            assert!(std::ptr::eq(
                outcome.remaining[&later].as_ptr(),
                later_bucket
            ));
            assert_eq!(outcome.remaining[&later][0].attempts(), 1);
            assert!(matches!(
                outcome.remaining[&later][0].last_error(),
                __err if __err.is_closing()));
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

        let outcome = retry_metadata_debt_with_host(&host, debts);

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

        let outcome = retry_metadata_debt_with_host(&host, debts);

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

        let outcome = retry_metadata_debt_with_host(&host, debts);

        assert!(outcome.remaining.is_empty());
        assert_eq!(outcome.cleanup_issues.len(), 1);
        assert!(outcome.terminal.is_none());
        script.assert_calls(&[XLF_EVALUATE, XLF_SET_NAME]);
    }

    #[test]
    fn retry_reports_terminal_cleanup_after_the_last_committed_deletion() {
        for status in [XLRET_ABORT, XLRET_UNCALCED] {
            let script = CallbackScript::install([
                Reply::success(XLF_EVALUATE, XLOPER12::number(7.0)),
                Reply::success(XLF_SET_NAME, XLOPER12::boolean(true)).release_status(status),
            ]);
            let callbacks = HostCallbackSession::new();
            let host = RegistrationHost::new(&callbacks);
            let debts = debts([debt("TEST.OWNED", 7.0)]);

            let outcome = retry_metadata_debt_with_host(&host, debts);

            assert!(outcome.remaining.is_empty());
            assert_eq!(outcome.cleanup_issues.len(), 1);
            assert!(matches!(
                outcome.terminal.as_ref().map(crate::error::construct::view),
                Some(crate::error::construct::ErrorView::ExcelApi {
                    failure: crate::error::ExcelApiFailure::Suppressed(terminal),
                    ..
                }) if terminal == crate::error::callback_status_from_raw(status),
            ));
            assert!(host.registration_id("TEST.LATER").is_err());
            script.assert_calls(&[XLF_EVALUATE, XLF_SET_NAME]);
        }
    }

    #[test]
    fn retry_returns_terminal_failure_after_clearing_the_last_ledger_debt() {
        for status in [XLRET_ABORT, XLRET_UNCALCED] {
            let script = CallbackScript::install([
                Reply::success(XLF_EVALUATE, XLOPER12::number(7.0)),
                Reply::success(XLF_SET_NAME, XLOPER12::boolean(true)).release_status(status),
            ]);
            let callbacks = HostCallbackSession::new();
            let ledger = HostLedger::new();
            ledger.retain_metadata_debt(vec![debt("TEST.OWNED", 7.0)]);

            assert!(
                matches!((retry_metadata_debt(&ledger, &callbacks)).as_ref().map_err(crate::error::construct::view),
                    Err(crate::error::construct::ErrorView::ExcelApi {
                        failure: crate::error::ExcelApiFailure::Suppressed(terminal),
                        ..
                    }) if terminal == crate::error::callback_status_from_raw(status),
                )
            );
            assert!(!ledger.metadata_debt_status().has_pending_work());
            assert_eq!(
                callbacks.terminal_status(),
                Some(crate::error::callback_status_from_raw(status))
            );
            script.assert_calls(&[XLF_EVALUATE, XLF_SET_NAME]);
        }
    }

    #[test]
    fn retry_cannot_succeed_with_empty_debt_in_an_already_terminal_entrypoint() {
        for status in [ExcelCallbackStatus::Abort, ExcelCallbackStatus::Uncalced] {
            let script = CallbackScript::install([]);
            let callbacks = HostCallbackSession::new();
            callbacks.suppress_for_test(status);
            let ledger = HostLedger::new();

            assert!(
                matches!((retry_metadata_debt(&ledger, &callbacks)).as_ref().map_err(crate::error::construct::view),
                    Err(crate::error::construct::ErrorView::ExcelApi {
                        failure: crate::error::ExcelApiFailure::Suppressed(terminal),
                        ..
                    }) if terminal == status,
                )
            );
            let outcome = retry_metadata_debt_with_host(
                &RegistrationHost::new(&callbacks),
                MetadataDebts::new(),
            );
            assert!(outcome.terminal.is_some());
            assert!(outcome.remaining.is_empty());
            assert!(!ledger.metadata_debt_status().has_pending_work());
            script.assert_calls(&[]);
        }
    }
}
