use crate::RtdSubscription;
use crate::generation::RuntimeGeneration;
use crate::shutdown::{CleanupIssue, CleanupIssueKind, StopOutcome};
use crate::subscription::{RtdLimits, RuntimeShutdownOutcome, SubscriptionRuntime};

/// Issued after the corresponding slot is sealed, or canonical state proves
/// that no subscription service was published for the generation.
#[derive(Debug)]
pub(crate) struct SubscriptionsStopped {
    generation: Option<RuntimeGeneration>,
}

impl SubscriptionsStopped {
    const fn new(generation: Option<RuntimeGeneration>) -> Self {
        Self { generation }
    }

    pub(crate) fn absent_services(absent: &crate::lifecycle::AbsentGenerationServices) -> Self {
        Self::new(absent.generation())
    }

    pub(crate) const fn generation(&self) -> Option<RuntimeGeneration> {
        self.generation
    }
}

pub(crate) struct SubscriptionServiceSlot<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    service: xlfn_kernel::service_slot::GenerationServiceSlot<
        SubscriptionRuntimeConfig<E, C>,
        SubscriptionRuntime<E, C>,
        crate::XllError,
    >,
    #[cfg(any(test, feature = "refinement"))]
    observer: crate::shutdown_trace::ObservationSink,
}

pub(crate) type SubscriptionRuntimeRead<'a, E = (), C = crate::DynamicRtdSources<E>> =
    xlfn_kernel::service_slot::GenerationServiceRead<'a, SubscriptionRuntime<E, C>>;

struct SubscriptionRuntimeConfig<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    generation: RuntimeGeneration,
    limits: RtdLimits,
    sources: crate::subscription::SourceArena<E, C>,
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> SubscriptionServiceSlot<E, C> {
    pub(crate) const fn new() -> Self {
        Self {
            service: xlfn_kernel::service_slot::GenerationServiceSlot::new(),
            #[cfg(any(test, feature = "refinement"))]
            observer: crate::shutdown_trace::ObservationSink::new(),
        }
    }

    pub(crate) fn arm(
        &self,
        generation: RuntimeGeneration,
        limits: RtdLimits,
        sources: crate::subscription::SourceArena<E, C>,
    ) -> crate::XllResult<()> {
        self.service
            .arm(SubscriptionRuntimeConfig {
                generation,
                limits,
                sources,
            })
            .map_err(crate::error::map_service_slot_error)
    }

    #[inline]
    pub(crate) fn read(
        &self,
        host: crate::excel_rtd::RtdSubscriptionHost,
    ) -> crate::XllResult<SubscriptionRuntimeRead<'_, E, C>> {
        self.service
            .read(
                |config| {
                    Ok(Box::new(SubscriptionRuntime::with_host(
                        config.generation,
                        config.limits,
                        host,
                        config.sources,
                    )))
                },
                #[cfg(any(test, feature = "refinement"))]
                |runtime| {
                    if let Some(trace) = self.observer.trace_handle() {
                        runtime.set_trace_sink(trace);
                    }
                },
                #[cfg(not(any(test, feature = "refinement")))]
                |_| {},
            )
            .map_err(crate::error::map_service_slot_error)
    }

    #[inline]
    pub(crate) fn is_none(&self) -> bool {
        self.service.is_none()
    }

    /// Signal an existing runtime without initializing a cold slot or waiting
    /// for any service readers. Deferred disconnects may still retain those
    /// readers and an enclosing module-export admission.
    pub(crate) fn request_stop_before_export_drain(&self) {
        if let Some(runtime) = self.service.read_if_ready() {
            runtime.request_stop_before_host_drain();
        }
    }

    pub(crate) fn seal(
        &self,
        generation: Option<RuntimeGeneration>,
    ) -> crate::XllResult<StopOutcome<SubscriptionsStopped>> {
        let generation = generation.or_else(|| {
            self.service
                .read_if_ready()
                .map(|runtime| runtime.generation)
        });
        let sealed = self
            .service
            .seal(
                move || StopOutcome {
                    certificate: SubscriptionsStopped::new(generation),
                    issues: Vec::new(),
                },
                |runtime| {
                    let outcome = crate::excel_rtd::shutdown_subscriptions(runtime)?;
                    let issues = match outcome {
                        RuntimeShutdownOutcome::Clean => Vec::new(),
                        RuntimeShutdownOutcome::PublisherPanicked => vec![CleanupIssue {
                            component: "RTD publisher",
                            kind: CleanupIssueKind::WorkerPanickedAfterJoin,
                            error: crate::XllError::PANIC,
                        }],
                    };
                    Ok(StopOutcome {
                        certificate: SubscriptionsStopped::new(Some(runtime.generation)),
                        issues,
                    })
                },
            )
            .map_err(crate::error::map_service_slot_error)?;
        let (runtime, outcome) = sealed.into_parts();
        drop(runtime);
        Ok(outcome)
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.observer.set_trace_sink(triomphe::Arc::clone(&trace));
        self.service.with_published(|runtime| {
            if let Some(runtime) = runtime {
                runtime.set_trace_sink(trace);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::excel_rtd::RtdSubscriptionHost;
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::thread;

    fn generation() -> RuntimeGeneration {
        RuntimeGeneration::ONE
    }

    #[test]
    fn subscription_service_slot_reuses_published_runtime() {
        let slot = SubscriptionServiceSlot::<()>::new();
        slot.arm(
            generation(),
            RtdLimits::standard(),
            crate::subscription::SourceArena::empty(generation()),
        )
        .unwrap();

        let first = slot.read(RtdSubscriptionHost::detached()).unwrap();
        let second = slot.read(RtdSubscriptionHost::detached()).unwrap();

        assert!(std::ptr::eq(&*first, &*second));
    }

    #[test]
    fn subscription_service_slot_initializes_once_under_contention() {
        let slot = Arc::new(SubscriptionServiceSlot::<()>::new());
        slot.arm(
            generation(),
            RtdLimits::standard(),
            crate::subscription::SourceArena::empty(generation()),
        )
        .unwrap();
        let barrier = Arc::new(Barrier::new(8));
        let mut handles = Vec::new();

        for _ in 0..8 {
            let slot = Arc::clone(&slot);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                let read = slot.read(RtdSubscriptionHost::detached()).unwrap();
                std::ptr::from_ref(&*read).addr()
            }));
        }

        let first_ptr = handles.remove(0).join().unwrap();
        for handle in handles {
            let ptr = handle.join().unwrap();
            assert_eq!(first_ptr, ptr);
        }
    }

    #[test]
    fn subscription_service_slot_seal_unpublishes_runtime() {
        let slot = SubscriptionServiceSlot::<()>::new();
        slot.arm(
            generation(),
            RtdLimits::standard(),
            crate::subscription::SourceArena::empty(generation()),
        )
        .unwrap();
        let read = slot.read(RtdSubscriptionHost::detached()).unwrap();
        drop(read);

        assert!(!slot.is_none());
        slot.seal(Some(generation())).unwrap();
        assert!(slot.is_none());
        assert!(matches!(
            slot.read(RtdSubscriptionHost::detached()),
            Err(__err) if __err.is_closing()));
    }

    #[test]
    #[cfg(not(windows))]
    fn joined_publisher_panic_is_reported_and_subscription_slot_is_reclaimed() {
        let slot = SubscriptionServiceSlot::<()>::new();
        slot.arm(
            generation(),
            RtdLimits::standard(),
            crate::subscription::SourceArena::empty(generation()),
        )
        .unwrap();
        let runtime = slot.read(RtdSubscriptionHost::detached()).unwrap();
        runtime.panic_publisher_for_test();
        drop(runtime);

        let stopped = slot.seal(Some(generation())).unwrap();
        assert_eq!(stopped.certificate.generation(), Some(generation()));
        assert_eq!(stopped.issues.len(), 1);
        assert_eq!(
            stopped.issues[0].kind,
            CleanupIssueKind::WorkerPanickedAfterJoin
        );
        assert!(stopped.issues[0].error.is_panic());
        assert!(slot.is_none());
    }

    #[test]
    fn subscription_service_slot_can_reopen_after_close() {
        let slot = SubscriptionServiceSlot::<()>::new();
        slot.arm(
            generation(),
            RtdLimits::standard(),
            crate::subscription::SourceArena::empty(generation()),
        )
        .unwrap();

        let first = slot.read(RtdSubscriptionHost::detached()).unwrap();
        let first_runtime_id = first.runtime_id;
        drop(first);

        slot.seal(Some(generation())).unwrap();

        slot.arm(
            generation(),
            RtdLimits::standard(),
            crate::subscription::SourceArena::empty(generation()),
        )
        .unwrap();
        let second = slot.read(RtdSubscriptionHost::detached()).unwrap();

        assert_ne!(first_runtime_id, second.runtime_id);
    }

    #[test]
    fn subscription_service_slot_seal_is_local_to_its_generation_bundle() {
        let slot = SubscriptionServiceSlot::<()>::new();
        assert!(matches!(
            slot.read(RtdSubscriptionHost::detached()),
            Err(__err) if __err.is_closing()));

        slot.arm(
            generation(),
            RtdLimits::standard(),
            crate::subscription::SourceArena::empty(generation()),
        )
        .unwrap();
        assert!(slot.read(RtdSubscriptionHost::detached()).is_ok());

        slot.seal(Some(generation())).unwrap();
        assert!(slot.is_none());
    }
}
