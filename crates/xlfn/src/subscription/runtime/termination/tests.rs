use super::*;
use crate::subscription::source::{RtdSink, RtdSource, SourceRegistration};
use crate::subscription::termination::TerminationPhase;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Source {
    stopped: Arc<AtomicUsize>,
    joined: Arc<AtomicUsize>,
    stop_panics: bool,
    join_fails: bool,
}

// SAFETY: no sink or worker is retained. Injected lifecycle failures therefore
// still satisfy the source-worker barrier, including error/unwind paths.
unsafe impl RtdSource for Source {
    type Value = i32;
    type Subscription = ();

    fn subscribe(
        &self,
        _: &crate::subscription::topic::RtdTopic,
        _: RtdSink<i32>,
    ) -> XllResult<()> {
        Ok(())
    }

    fn request_stop(&self) {
        self.stopped.fetch_add(1, Ordering::Relaxed);
        assert!(!self.stop_panics, "injected source stop failure");
    }

    fn shutdown_and_wait(&self) -> XllResult<()> {
        self.joined.fetch_add(1, Ordering::Relaxed);
        if self.join_fails {
            Err(XllError::Overloaded)
        } else {
            Ok(())
        }
    }
}

fn runtime_with_sources(
    stop_panics: bool,
    join_fails: bool,
) -> (
    SubscriptionRuntime<crate::excel_rtd::RtdSubscriptionHost>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let stopped = Arc::new(AtomicUsize::new(0));
    let joined = Arc::new(AtomicUsize::new(0));
    let sources = SourceRegistration::new(RuntimeGeneration::new(1).unwrap());
    for index in 0..2 {
        sources
            .register(Source {
                stopped: Arc::clone(&stopped),
                joined: Arc::clone(&joined),
                stop_panics: stop_panics && index == 0,
                join_fails: join_fails && index == 0,
            })
            .unwrap();
    }
    (
        SubscriptionRuntime::with_sources_for_internal(sources.finish()),
        stopped,
        joined,
    )
}

fn claim<H: SubscriptionHost, E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    runtime: &SubscriptionRuntime<H, E, C>,
) -> RuntimeTermination<'_, H, E, C, Claimed> {
    match RuntimeTermination::begin(runtime) {
        RuntimeTerminationAdmission::Owner(owner) => owner,
        _ => panic!("expected sole runtime termination owner"),
    }
}

#[test]
fn miri_runtime_completion_remains_pending_until_catalog_finalization() {
    let (runtime, stopped, joined) = runtime_with_sources(false, false);
    let claimed = claim(&runtime);
    assert_eq!(
        runtime.termination_coordinator.phase(),
        TerminationPhase::Terminating
    );
    let stopped_owner = claimed.request_stop();
    assert_eq!(stopped.load(Ordering::Relaxed), 2);
    assert_eq!(joined.load(Ordering::Relaxed), 0);
    let drained = stopped_owner.drain_sink_users();
    assert_eq!(joined.load(Ordering::Relaxed), 0);
    let joined_owner = drained.join_workers().unwrap();
    assert_eq!(joined.load(Ordering::Relaxed), 2);
    assert_eq!(
        runtime.termination_coordinator.phase(),
        TerminationPhase::Terminating
    );
    joined_owner.finish().unwrap();
    assert_eq!(
        runtime.termination_coordinator.phase(),
        TerminationPhase::Terminated
    );
    runtime.close().unwrap();
    assert_eq!(joined.load(Ordering::Relaxed), 2);
}

#[test]
fn miri_executed_source_barriers_preserve_errors_without_skipping_later_sources() {
    for stop_panics in [false, true] {
        let (runtime, stopped, joined) = runtime_with_sources(stop_panics, true);
        let finished = claim(&runtime)
            .request_stop()
            .drain_sink_users()
            .join_workers()
            .unwrap();
        assert_eq!(stopped.load(Ordering::Relaxed), 2);
        assert_eq!(joined.load(Ordering::Relaxed), 2);
        assert_eq!(
            runtime.termination_coordinator.phase(),
            TerminationPhase::Terminating
        );
        let result = finished.finish();
        if stop_panics {
            assert!(matches!(result, Err(XllError::Panic)));
        } else {
            assert!(matches!(result, Err(XllError::Overloaded)));
        }
        assert_eq!(
            runtime.termination_coordinator.phase(),
            TerminationPhase::Terminated
        );
        assert!(runtime.close().is_err());
        assert_eq!(joined.load(Ordering::Relaxed), 2);
    }
}

#[test]
fn miri_source_dispatch_preflight_failure_never_certifies_executed_barriers() {
    let (runtime, stopped, joined) = runtime_with_sources(false, false);
    runtime
        .fail_source_shutdown_preflight
        .store(true, Ordering::Relaxed);
    assert!(matches!(
        claim(&runtime)
            .request_stop()
            .drain_sink_users()
            .join_workers(),
        Err(XllError::Overloaded)
    ));
    assert_eq!(stopped.load(Ordering::Relaxed), 2);
    assert_eq!(joined.load(Ordering::Relaxed), 0);
    assert_eq!(
        runtime.termination_coordinator.phase(),
        TerminationPhase::Failed
    );
    assert!(matches!(runtime.close(), Err(XllError::Overloaded)));
    assert_eq!(joined.load(Ordering::Relaxed), 0);
}

fn abandon<S>(
    transaction: RuntimeTermination<
        '_,
        crate::excel_rtd::RtdSubscriptionHost,
        (),
        crate::DynamicRtdSources<()>,
        S,
    >,
    panic: bool,
) {
    assert!(!panic, "injected stage abandonment");
    drop(transaction);
}

#[test]
fn miri_runtime_stage_drop_or_panic_never_publishes_success() {
    for panic in [false, true] {
        for stage in 0..4 {
            let runtime = SubscriptionRuntime::<crate::excel_rtd::RtdSubscriptionHost>::new();
            let result = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
                let claimed = claim(&runtime);
                if stage == 0 {
                    return abandon(claimed, panic);
                }
                let stopped = claimed.request_stop();
                if stage == 1 {
                    return abandon(stopped, panic);
                }
                let drained = stopped.drain_sink_users();
                if stage == 2 {
                    return abandon(drained, panic);
                }
                abandon(drained.join_workers().unwrap(), panic);
            }));
            assert_eq!(result.is_err(), panic);
            assert_eq!(
                runtime.termination_coordinator.phase(),
                TerminationPhase::Failed
            );
            assert!(matches!(runtime.close(), Err(XllError::Panic)));
        }
    }
}

// Forget intentionally retains the transaction's owner, so dropping its issuer
// would wait forever. Keep that empty issuer as well; no completion is minted.
#[test]
#[cfg(not(miri))]
#[allow(
    clippy::mem_forget,
    reason = "The native-only regression intentionally forgets each stage and retains its empty issuer to verify that completion remains pending"
)]
fn forgotten_runtime_termination_keeps_completion_pending() {
    for stage in 0..4 {
        let runtime = std::mem::ManuallyDrop::new(SubscriptionRuntime::<
            crate::excel_rtd::RtdSubscriptionHost,
        >::new());
        let claimed = claim(&runtime);
        if stage == 0 {
            std::mem::forget(claimed);
        } else {
            let stopped = claimed.request_stop();
            if stage == 1 {
                std::mem::forget(stopped);
            } else {
                let drained = stopped.drain_sink_users();
                if stage == 2 {
                    std::mem::forget(drained);
                } else {
                    std::mem::forget(drained.join_workers().unwrap());
                }
            }
        }
        assert_eq!(
            runtime.termination_coordinator.phase(),
            TerminationPhase::Terminating
        );
    }
}
