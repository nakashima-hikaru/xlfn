use crate::generation::RuntimeGeneration;
use crate::shutdown::SubscriptionsStopped;
use crate::subscription::{RtdLimits, SubscriptionRuntime};

pub(crate) struct SubscriptionServiceSlot {
    service: xlfn_kernel::service_slot::GenerationServiceSlot<
        SubscriptionRuntimeConfig,
        SubscriptionRuntime,
        crate::XllError,
    >,
    observer: crate::shutdown_trace::ObservationSink,
}

pub(crate) type SubscriptionRuntimeRead<'a> =
    xlfn_kernel::service_slot::GenerationServiceRead<'a, SubscriptionRuntime>;

struct SubscriptionRuntimeConfig {
    generation: RuntimeGeneration,
    limits: RtdLimits,
    sources: crate::subscription::SourceArena,
}

impl SubscriptionServiceSlot {
    pub(crate) const fn new() -> Self {
        Self {
            service: xlfn_kernel::service_slot::GenerationServiceSlot::new(),
            observer: crate::shutdown_trace::ObservationSink::new(),
        }
    }

    pub(crate) fn arm(
        &self,
        generation: RuntimeGeneration,
        limits: RtdLimits,
        sources: crate::subscription::SourceArena,
    ) -> crate::XllResult<()> {
        self.service
            .arm(SubscriptionRuntimeConfig {
                generation,
                limits,
                sources,
            })
            .map_err(crate::error::map_service_slot_error)
    }

    pub(crate) fn disarm(&self) -> crate::XllResult<()> {
        self.service
            .disarm()
            .map_err(crate::error::map_service_slot_error)
    }

    #[inline]
    pub(crate) fn read(
        &self,
        host: crate::excel_rtd::RtdSubscriptionHost,
    ) -> crate::XllResult<SubscriptionRuntimeRead<'_>> {
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
                |_runtime| {
                    if let Some(trace) = self.observer.trace_handle() {
                        _runtime.set_trace_sink(trace);
                    }
                },
            )
            .map_err(crate::error::map_service_slot_error)
    }

    #[inline]
    pub(crate) fn is_none(&self) -> bool {
        self.service.is_none()
    }

    pub(crate) fn seal(
        &self,
        generation: Option<RuntimeGeneration>,
    ) -> crate::XllResult<SubscriptionsStopped> {
        let generation = generation.or_else(|| {
            self.service
                .read_if_ready()
                .map(|runtime| runtime.generation)
        });
        let sealed = self
            .service
            .seal(
                move || SubscriptionsStopped::issue(generation),
                |runtime| {
                    crate::excel_rtd::shutdown_subscriptions(runtime)
                        .map(|()| SubscriptionsStopped::issue(Some(runtime.generation)))
                },
            )
            .map_err(crate::error::map_service_slot_error)?;
        let (runtime, stopped) = sealed.into_parts();
        drop(runtime);
        Ok(stopped)
    }

    #[allow(
        dead_code,
        reason = "trace wiring is used only when the runtime observer is enabled"
    )]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.observer.set_trace_sink(std::sync::Arc::clone(&trace));
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
        RuntimeGeneration::new(1).expect("test generation is non-zero")
    }

    #[test]
    fn subscription_service_slot_reuses_published_runtime() {
        let slot = SubscriptionServiceSlot::new();
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
        let slot = Arc::new(SubscriptionServiceSlot::new());
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
        let slot = SubscriptionServiceSlot::new();
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
            Err(crate::XllError::Closing)
        ));
    }

    #[test]
    fn subscription_service_slot_can_reopen_after_close() {
        let slot = SubscriptionServiceSlot::new();
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
        let slot = SubscriptionServiceSlot::new();
        assert!(matches!(
            slot.read(RtdSubscriptionHost::detached()),
            Err(crate::XllError::Closing)
        ));

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
