use crate::generation::RuntimeGeneration;
use crate::shutdown::SubscriptionsStopped;
use crate::subscription::{RtdLimits, SubscriptionRuntime};
use std::sync::Arc;

pub(crate) struct SubscriptionServiceSlot {
    service: xlfn_kernel::service_slot::GenerationServiceSlot<
        SubscriptionRuntimeConfig,
        SubscriptionRuntime,
        crate::XllError,
    >,
    #[cfg(any(test, feature = "refinement"))]
    trace: std::sync::OnceLock<crate::shutdown_trace::ShutdownTraceHandle>,
}

pub(crate) type SubscriptionRuntimeRead =
    xlfn_kernel::service_slot::GenerationServiceRead<SubscriptionRuntime>;

#[derive(Clone, Copy)]
struct SubscriptionRuntimeConfig {
    generation: RuntimeGeneration,
    limits: RtdLimits,
}

impl SubscriptionServiceSlot {
    pub(crate) const fn new() -> Self {
        Self {
            service: xlfn_kernel::service_slot::GenerationServiceSlot::new(),
            #[cfg(any(test, feature = "refinement"))]
            trace: std::sync::OnceLock::new(),
        }
    }

    pub(crate) fn arm(
        &self,
        generation: RuntimeGeneration,
        limits: RtdLimits,
    ) -> crate::XllResult<()> {
        self.service
            .arm(SubscriptionRuntimeConfig { generation, limits })
            .map_err(crate::runtime_components::map_service_error)
    }

    pub(crate) fn disarm(&self) -> crate::XllResult<()> {
        self.service
            .disarm()
            .map_err(crate::runtime_components::map_service_error)
    }

    #[inline]
    pub(crate) fn read(
        &self,
        host: crate::excel_rtd::RtdSubscriptionHost,
    ) -> crate::XllResult<SubscriptionRuntimeRead> {
        self.service
            .read(
                |config| {
                    Ok(Arc::new(SubscriptionRuntime::with_host(
                        config.generation,
                        config.limits,
                        host,
                    )))
                },
                |_runtime| {
                    #[cfg(any(test, feature = "refinement"))]
                    if let Some(trace) = self.trace.get() {
                        _runtime.set_trace_sink(std::sync::Arc::clone(trace));
                    }
                },
            )
            .map_err(crate::runtime_components::map_service_error)
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
                .map(|runtime| runtime.as_arc().generation)
        });
        self.service
            .seal(
                crate::XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_SLOTS,
                },
                move || SubscriptionsStopped::issue(generation),
                |runtime| {
                    crate::excel_rtd::shutdown_subscriptions(Arc::clone(&runtime))
                        .map(|()| SubscriptionsStopped::issue(Some(runtime.generation)))
                },
            )
            .map_err(crate::runtime_components::map_service_error)
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        let _ = self.trace.set(std::sync::Arc::clone(&trace));
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
    use std::sync::Barrier;
    use std::thread;

    fn generation() -> RuntimeGeneration {
        RuntimeGeneration::new(1).expect("test generation is non-zero")
    }

    #[test]
    fn subscription_service_slot_reuses_published_runtime() {
        let slot = SubscriptionServiceSlot::new();
        slot.arm(generation(), RtdLimits::standard()).unwrap();

        let first = slot.read(RtdSubscriptionHost::detached()).unwrap();
        let second = slot.read(RtdSubscriptionHost::detached()).unwrap();

        assert!(Arc::ptr_eq(first.as_arc(), second.as_arc()));
    }

    #[test]
    fn subscription_service_slot_initializes_once_under_contention() {
        let slot = Arc::new(SubscriptionServiceSlot::new());
        slot.arm(generation(), RtdLimits::standard()).unwrap();
        let barrier = Arc::new(Barrier::new(8));
        let mut handles = Vec::new();

        for _ in 0..8 {
            let slot = Arc::clone(&slot);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                let read = slot.read(RtdSubscriptionHost::detached()).unwrap();
                Arc::as_ptr(read.as_arc()).addr()
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
        slot.arm(generation(), RtdLimits::standard()).unwrap();
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
        slot.arm(generation(), RtdLimits::standard()).unwrap();

        let first = slot.read(RtdSubscriptionHost::detached()).unwrap();
        let first_runtime = Arc::clone(first.as_arc());
        drop(first);

        slot.seal(Some(generation())).unwrap();

        slot.arm(generation(), RtdLimits::standard()).unwrap();
        let second = slot.read(RtdSubscriptionHost::detached()).unwrap();

        assert!(!Arc::ptr_eq(&first_runtime, second.as_arc()));
    }

    #[test]
    fn subscription_service_slot_seal_is_local_to_its_generation_bundle() {
        let slot = SubscriptionServiceSlot::new();
        assert!(matches!(
            slot.read(RtdSubscriptionHost::detached()),
            Err(crate::XllError::Closing)
        ));

        slot.arm(generation(), RtdLimits::standard()).unwrap();
        assert!(slot.read(RtdSubscriptionHost::detached()).is_ok());

        slot.seal(Some(generation())).unwrap();
        assert!(slot.is_none());
    }
}
