use super::{RtdLimits, SubscriptionRuntime};
use crate::generation::RuntimeGeneration;
use arc_swap::{ArcSwapOption, Guard};
use parking_lot::Mutex;
use std::mem::ManuallyDrop;
use std::sync::Arc;

pub(crate) struct SubscriptionRuntimeSlot {
    published: ArcSwapOption<SubscriptionRuntime>,
    transition: Mutex<()>,
    state: Mutex<SubscriptionRuntimeSlotState>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: std::sync::OnceLock<crate::shutdown_refinement::GhostHandle>,
}

type SubscriptionRuntimeSlotState =
    crate::runtime_components::GenerationServiceState<RtdLimits, SubscriptionRuntime>;

pub(crate) struct SubscriptionRuntimeRead {
    guard: Guard<Option<Arc<SubscriptionRuntime>>>,
}

impl std::ops::Deref for SubscriptionRuntimeRead {
    type Target = SubscriptionRuntime;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.guard
            .as_ref()
            .expect("SubscriptionRuntimeRead always contains a runtime")
            .as_ref()
    }
}

impl SubscriptionRuntimeRead {
    #[inline]
    pub(crate) fn as_arc(&self) -> &Arc<SubscriptionRuntime> {
        self.guard
            .as_ref()
            .expect("SubscriptionRuntimeRead always contains a runtime")
    }
}

impl SubscriptionRuntimeSlot {
    pub(crate) const fn new() -> Self {
        Self {
            published: ArcSwapOption::const_empty(),
            transition: Mutex::new(()),
            state: Mutex::new(SubscriptionRuntimeSlotState::Closed),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: std::sync::OnceLock::new(),
        }
    }

    pub(crate) fn arm(
        &self,
        generation: RuntimeGeneration,
        limits: RtdLimits,
    ) -> crate::XllResult<()> {
        let mut state = self.state.lock();
        if !matches!(*state, SubscriptionRuntimeSlotState::Closed) {
            return Err(crate::XllError::Closing);
        }
        *state = SubscriptionRuntimeSlotState::Cold {
            generation,
            config: limits,
        };
        Ok(())
    }

    pub(crate) fn disarm(&self, generation: RuntimeGeneration) -> crate::XllResult<()> {
        let mut state = self.state.lock();
        match &*state {
            SubscriptionRuntimeSlotState::Cold {
                generation: active, ..
            } if *active == generation => {
                *state = SubscriptionRuntimeSlotState::Closed;
                Ok(())
            }
            SubscriptionRuntimeSlotState::Closed => Ok(()),
            _ => Err(crate::XllError::Closing),
        }
    }

    #[inline]
    pub(crate) fn read(&self) -> crate::XllResult<SubscriptionRuntimeRead> {
        let guard = self.published.load();

        if guard.is_some() {
            return Ok(SubscriptionRuntimeRead { guard });
        }

        drop(guard);
        self.read_slow()
    }

    #[cold]
    fn read_slow(&self) -> crate::XllResult<SubscriptionRuntimeRead> {
        let _transition = self.transition.lock();
        let mut state = self.state.lock();

        let guard = self.published.load();
        if guard.is_some() {
            return Ok(SubscriptionRuntimeRead { guard });
        }
        drop(guard);

        match &*state {
            SubscriptionRuntimeSlotState::Cold {
                generation,
                config: limits,
            } => {
                let generation = *generation;
                let limits = *limits;
                *state = SubscriptionRuntimeSlotState::Initializing { generation };
                drop(state);

                let runtime = Arc::new(SubscriptionRuntime::with_module_ingress(limits));
                #[cfg(any(test, feature = "shutdown-refinement"))]
                if let Some(ghost) = self.ghost.get() {
                    runtime.set_ghost(ghost.clone());
                }
                self.published.store(Some(runtime));

                let mut state = self.state.lock();
                *state = SubscriptionRuntimeSlotState::Ready { generation };
                Ok(SubscriptionRuntimeRead {
                    guard: self.published.load(),
                })
            }
            SubscriptionRuntimeSlotState::Initializing { .. }
            | SubscriptionRuntimeSlotState::Sealing { .. } => {
                unreachable!("subscription slot transition is serialized");
            }
            SubscriptionRuntimeSlotState::InitFaulted { error, .. }
            | SubscriptionRuntimeSlotState::TeardownFaulted { error, .. } => Err(error.clone()),
            SubscriptionRuntimeSlotState::Closed => Err(crate::XllError::Closing),
            SubscriptionRuntimeSlotState::Ready { .. } => {
                unreachable!("published subscription runtime missing");
            }
        }
    }

    #[inline]
    pub(crate) fn is_none(&self) -> bool {
        self.published.load().is_none()
            && matches!(
                *self.state.lock(),
                SubscriptionRuntimeSlotState::Closed
                    | SubscriptionRuntimeSlotState::InitFaulted { .. }
            )
    }

    pub(crate) fn seal(&self, generation: Option<RuntimeGeneration>) -> crate::XllResult<()> {
        let _transition = self.transition.lock();
        let runtime = {
            let mut state = self.state.lock();
            match &*state {
                SubscriptionRuntimeSlotState::Ready { generation: active } => {
                    if generation != Some(*active) {
                        return Err(crate::XllError::Closing);
                    }
                    let runtime = self.published.swap(None).ok_or(crate::XllError::Internal {
                        diagnostic_id: crate::DiagnosticId::HANDLE_SLOT,
                    })?;
                    *state = SubscriptionRuntimeSlotState::Sealing {
                        generation: *active,
                    };
                    Some(runtime)
                }
                SubscriptionRuntimeSlotState::Cold {
                    generation: active, ..
                }
                | SubscriptionRuntimeSlotState::InitFaulted {
                    generation: active, ..
                } => {
                    if generation != Some(*active) {
                        return Err(crate::XllError::Closing);
                    }
                    *state = SubscriptionRuntimeSlotState::Closed;
                    return Ok(());
                }
                SubscriptionRuntimeSlotState::Closed => return Ok(()),
                SubscriptionRuntimeSlotState::TeardownFaulted { error, .. } => {
                    return Err(error.clone());
                }
                SubscriptionRuntimeSlotState::Initializing { .. }
                | SubscriptionRuntimeSlotState::Sealing { .. } => {
                    return Err(crate::XllError::Closing);
                }
            }
        };

        let runtime = runtime.expect("ready subscription slot must publish a runtime");
        let result = crate::rtd::shutdown_subscriptions(Arc::clone(&runtime));
        let mut state = self.state.lock();
        match result {
            Ok(()) => {
                *state = SubscriptionRuntimeSlotState::Closed;
                Ok(())
            }
            Err(error) => {
                *state = SubscriptionRuntimeSlotState::TeardownFaulted {
                    generation: generation.expect("a live subscription runtime has a generation"),
                    error: error.clone(),
                    runtime: ManuallyDrop::new(runtime),
                };
                Err(error)
            }
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        let _ = self.ghost.set(ghost.clone());

        let runtime = self.published.load();
        if let Some(runtime) = runtime.as_ref() {
            runtime.set_ghost(ghost);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::thread;

    fn generation(raw: u64) -> RuntimeGeneration {
        RuntimeGeneration::new(raw).expect("test generation is non-zero")
    }

    #[test]
    fn subscription_slot_reuses_published_runtime() {
        let slot = SubscriptionRuntimeSlot::new();
        slot.arm(generation(1), RtdLimits::standard()).unwrap();

        let first = slot.read().unwrap();
        let second = slot.read().unwrap();

        assert!(Arc::ptr_eq(first.as_arc(), second.as_arc()));
    }

    #[test]
    fn subscription_slot_initializes_once_under_contention() {
        let slot = Arc::new(SubscriptionRuntimeSlot::new());
        slot.arm(generation(1), RtdLimits::standard()).unwrap();
        let barrier = Arc::new(Barrier::new(8));
        let mut handles = Vec::new();

        for _ in 0..8 {
            let slot = Arc::clone(&slot);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                let read = slot.read().unwrap();
                Arc::as_ptr(read.as_arc()) as usize
            }));
        }

        let first_ptr = handles.remove(0).join().unwrap();
        for handle in handles {
            let ptr = handle.join().unwrap();
            assert_eq!(first_ptr, ptr);
        }
    }

    #[test]
    fn subscription_slot_seal_unpublishes_runtime() {
        let slot = SubscriptionRuntimeSlot::new();
        slot.arm(generation(1), RtdLimits::standard()).unwrap();
        let read = slot.read().unwrap();
        drop(read);

        assert!(!slot.is_none());
        slot.seal(Some(generation(1))).unwrap();
        assert!(slot.is_none());
        assert!(matches!(slot.read(), Err(crate::XllError::Closing)));
    }

    #[test]
    fn subscription_slot_can_reopen_after_close() {
        let slot = SubscriptionRuntimeSlot::new();
        slot.arm(generation(1), RtdLimits::standard()).unwrap();

        let first = slot.read().unwrap();
        let first_runtime = Arc::clone(first.as_arc());
        drop(first);

        slot.seal(Some(generation(1))).unwrap();

        slot.arm(generation(2), RtdLimits::standard()).unwrap();
        let second = slot.read().unwrap();

        assert!(!Arc::ptr_eq(&first_runtime, second.as_arc()));
    }

    #[test]
    fn subscription_slot_requires_matching_generation_for_seal() {
        let slot = SubscriptionRuntimeSlot::new();
        assert!(matches!(slot.read(), Err(crate::XllError::Closing)));

        slot.arm(generation(7), RtdLimits::standard()).unwrap();
        assert!(matches!(
            slot.seal(Some(generation(6))),
            Err(crate::XllError::Closing)
        ));
        assert!(slot.read().is_ok());
        assert!(matches!(
            slot.disarm(generation(6)),
            Err(crate::XllError::Closing)
        ));

        slot.seal(Some(generation(7))).unwrap();
        assert!(slot.is_none());
    }
}
