use crate::generation::RuntimeGeneration;
use crate::subscription::{RtdLimits, SubscriptionRuntime};
use std::sync::Arc;

pub(crate) struct SubscriptionServiceSlot {
    service: xlfn_kernel::service_slot::GenerationServiceSlot<
        SubscriptionRuntimeConfig,
        SubscriptionRuntime,
        crate::XllError,
    >,
    #[cfg(any(test, feature = "refinement"))]
    ghost: std::sync::OnceLock<crate::shutdown_refinement::GhostHandle>,
}

#[derive(Debug)]
pub(crate) struct SubscriptionsStopped {
    _private: (),
}

impl SubscriptionsStopped {
    pub(crate) fn new() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    pub(crate) const fn for_test() -> Self {
        Self { _private: () }
    }
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
            ghost: std::sync::OnceLock::new(),
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
    pub(crate) fn read(&self) -> crate::XllResult<SubscriptionRuntimeRead> {
        self.service
            .read(
                |config| {
                    Ok(Arc::new(SubscriptionRuntime::with_host(
                        config.generation,
                        config.limits,
                        crate::rtd::RtdSubscriptionHost::production(),
                    )))
                },
                |_runtime| {
                    #[cfg(any(test, feature = "refinement"))]
                    if let Some(ghost) = self.ghost.get() {
                        _runtime.set_ghost(ghost.clone());
                    }
                },
            )
            .map_err(crate::runtime_components::map_service_error)
    }

    #[inline]
    pub(crate) fn is_none(&self) -> bool {
        self.service.is_none()
    }

    pub(crate) fn seal(&self) -> crate::XllResult<SubscriptionsStopped> {
        self.service
            .seal(
                crate::XllError::Internal {
                    diagnostic_id: crate::error::DiagnosticId::RTD_SLOTS,
                },
                SubscriptionsStopped::new,
                |runtime| {
                    crate::rtd::shutdown_subscriptions(Arc::clone(&runtime))
                        .map(|()| SubscriptionsStopped::new())
                },
            )
            .map_err(crate::runtime_components::map_service_error)
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        let _ = self.ghost.set(ghost.clone());
        self.service.with_published(|runtime| {
            if let Some(runtime) = runtime {
                runtime.set_ghost(ghost);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::thread;

    fn generation() -> RuntimeGeneration {
        RuntimeGeneration::new(1).expect("test generation is non-zero")
    }

    #[test]
    fn subscription_service_slot_reuses_published_runtime() {
        let slot = SubscriptionServiceSlot::new();
        slot.arm(generation(), RtdLimits::standard()).unwrap();

        let first = slot.read().unwrap();
        let second = slot.read().unwrap();

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
                let read = slot.read().unwrap();
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
        let read = slot.read().unwrap();
        drop(read);

        assert!(!slot.is_none());
        slot.seal().unwrap();
        assert!(slot.is_none());
        assert!(matches!(slot.read(), Err(crate::XllError::Closing)));
    }

    #[test]
    fn subscription_service_slot_can_reopen_after_close() {
        let slot = SubscriptionServiceSlot::new();
        slot.arm(generation(), RtdLimits::standard()).unwrap();

        let first = slot.read().unwrap();
        let first_runtime = Arc::clone(first.as_arc());
        drop(first);

        slot.seal().unwrap();

        slot.arm(generation(), RtdLimits::standard()).unwrap();
        let second = slot.read().unwrap();

        assert!(!Arc::ptr_eq(&first_runtime, second.as_arc()));
    }

    #[test]
    fn subscription_service_slot_seal_is_local_to_its_generation_bundle() {
        let slot = SubscriptionServiceSlot::new();
        assert!(matches!(slot.read(), Err(crate::XllError::Closing)));

        slot.arm(generation(), RtdLimits::standard()).unwrap();
        assert!(slot.read().is_ok());

        slot.seal().unwrap();
        assert!(slot.is_none());
    }
}
