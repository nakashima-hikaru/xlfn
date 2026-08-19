use super::{RtdLimits, SubscriptionRuntime};
use arc_swap::{ArcSwapOption, Guard};
use parking_lot::Mutex;
use std::sync::Arc;

pub(crate) struct SubscriptionRuntimeSlot {
    published: ArcSwapOption<SubscriptionRuntime>,
    transition: Mutex<()>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: std::sync::OnceLock<crate::shutdown_refinement::GhostHandle>,
}

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
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: std::sync::OnceLock::new(),
        }
    }

    #[inline]
    pub(crate) fn read(&self, limits: RtdLimits) -> SubscriptionRuntimeRead {
        let guard = self.published.load();

        if guard.is_some() {
            return SubscriptionRuntimeRead { guard };
        }

        drop(guard);
        self.read_slow(limits)
    }

    #[cold]
    fn read_slow(&self, limits: RtdLimits) -> SubscriptionRuntimeRead {
        let _transition = self.transition.lock();

        let guard = self.published.load();
        if guard.is_some() {
            return SubscriptionRuntimeRead { guard };
        }
        drop(guard);

        let runtime = Arc::new(SubscriptionRuntime::with_module_ingress(limits));

        #[cfg(any(test, feature = "shutdown-refinement"))]
        if let Some(ghost) = self.ghost.get() {
            runtime.set_ghost(ghost.clone());
        }

        self.published.store(Some(runtime));

        let guard = self.published.load();
        debug_assert!(guard.is_some());

        SubscriptionRuntimeRead { guard }
    }

    pub(crate) fn take(&self) -> Option<Arc<SubscriptionRuntime>> {
        let _transition = self.transition.lock();
        self.published.swap(None)
    }

    #[inline]
    pub(crate) fn is_none(&self) -> bool {
        self.published.load().is_none()
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

    #[test]
    fn subscription_slot_reuses_published_runtime() {
        let slot = SubscriptionRuntimeSlot::new();

        let first = slot.read(RtdLimits::standard());
        let second = slot.read(RtdLimits::standard());

        assert!(Arc::ptr_eq(first.as_arc(), second.as_arc()));
    }

    #[test]
    fn subscription_slot_initializes_once_under_contention() {
        let slot = Arc::new(SubscriptionRuntimeSlot::new());
        let barrier = Arc::new(Barrier::new(8));
        let mut handles = Vec::new();

        for _ in 0..8 {
            let slot = Arc::clone(&slot);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                let read = slot.read(RtdLimits::standard());
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
    fn subscription_slot_take_unpublishes_runtime() {
        let slot = SubscriptionRuntimeSlot::new();
        let read = slot.read(RtdLimits::standard());
        let runtime_ptr = Arc::as_ptr(read.as_arc());
        drop(read);

        assert!(!slot.is_none());
        let taken = slot.take().unwrap();
        assert_eq!(Arc::as_ptr(&taken), runtime_ptr);
        assert!(slot.is_none());
        assert!(slot.take().is_none());
    }

    #[test]
    fn subscription_slot_can_reopen_after_close() {
        let slot = SubscriptionRuntimeSlot::new();

        let first = slot.read(RtdLimits::standard());
        let first_ptr = Arc::as_ptr(first.as_arc());
        drop(first);

        let taken = slot.take().unwrap();
        assert_eq!(Arc::as_ptr(&taken), first_ptr);

        let second = slot.read(RtdLimits::standard());
        let second_ptr = Arc::as_ptr(second.as_arc());

        assert_ne!(first_ptr, second_ptr);
        drop(taken);
    }
}
