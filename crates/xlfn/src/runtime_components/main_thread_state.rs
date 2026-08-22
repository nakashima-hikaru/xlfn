//! Main-thread-only ownership for add-in lifecycle state.
//!
//! `SharedState` is published in the generation root and is consequently
//! required to be `Send + Sync`. Lifecycle state is different: it is touched
//! only by the Excel lifecycle thread and may own apartment-affine or
//! otherwise non-`Send` resources. The runtime therefore keeps it in this
//! thread-local slot rather than putting it behind a cross-thread lock.

use std::any::Any;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::Addin;

struct Entry {
    runtime_address: usize,
    state: Option<Box<dyn Any>>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        // Lifecycle state is terminally retained unless `take` explicitly
        // transfers it after successful cleanup. This prevents a TLS
        // destructor from running add-in code after a failed unload proof.
        if let Some(state) = self.state.take() {
            std::mem::forget(state);
        }
    }
}

thread_local! {
    static LIFECYCLE_STATES: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
}

static NEXT_RUNTIME_KEY: AtomicUsize = AtomicUsize::new(1);

/// A runtime-owned key for the current thread's lifecycle state.
pub(crate) struct MainThreadStateSlot<A: Addin> {
    runtime_key: AtomicUsize,
    _marker: PhantomData<fn() -> A::LifecycleState>,
}

impl<A: Addin> MainThreadStateSlot<A> {
    pub(crate) const fn new() -> Self {
        Self {
            runtime_key: AtomicUsize::new(0),
            _marker: PhantomData,
        }
    }

    fn runtime_address(&self) -> usize {
        let current = self.runtime_key.load(Ordering::Relaxed);
        if current != 0 {
            return current;
        }
        let allocated = NEXT_RUNTIME_KEY.fetch_add(1, Ordering::Relaxed);
        match self
            .runtime_key
            .compare_exchange(0, allocated, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => allocated,
            Err(existing) => existing,
        }
    }

    /// Installs the one lifecycle state owned by this runtime generation.
    pub(crate) fn install(&self, state: A::LifecycleState) -> Result<(), A::LifecycleState> {
        let runtime_address = self.runtime_address();
        LIFECYCLE_STATES.with(|states| {
            let mut states = states.borrow_mut();
            if states
                .iter()
                .any(|entry| entry.runtime_address == runtime_address)
            {
                return Err(state);
            }
            states.push(Entry {
                runtime_address,
                state: Some(Box::new(state)),
            });
            Ok(())
        })
    }

    /// Runs a lifecycle operation while preserving ownership in the slot.
    /// Keeping the value installed across quiesce and cleanup means a failed
    /// boundary can intentionally retain it without running a destructor.
    pub(crate) fn with_mut<R>(
        &self,
        operation: impl FnOnce(&mut A::LifecycleState) -> R,
    ) -> Option<R> {
        let runtime_address = self.runtime_address();
        LIFECYCLE_STATES.with(|states| {
            let mut states = states.borrow_mut();
            let entry = states
                .iter_mut()
                .find(|entry| entry.runtime_address == runtime_address)?;
            let state = entry.state.as_mut()?.as_mut();
            Some(operation(state.downcast_mut::<A::LifecycleState>()?))
        })
    }

    /// Removes a successfully cleaned lifecycle state so its destructor runs
    /// on the lifecycle thread that created and quiesced it.
    pub(crate) fn take(&self) -> Option<A::LifecycleState> {
        let runtime_address = self.runtime_address();
        LIFECYCLE_STATES.with(|states| {
            let mut states = states.borrow_mut();
            let index = states
                .iter()
                .position(|entry| entry.runtime_address == runtime_address)?;
            let mut entry = states.swap_remove(index);
            let state = entry.state.take()?;
            std::mem::forget(entry);
            match state.downcast::<A::LifecycleState>() {
                Ok(state) => Some(*state),
                Err(state) => {
                    // The key is private and type-specific; a mismatch means
                    // an internal invariant was violated. Never drop an
                    // unknown lifecycle value on this path.
                    std::mem::forget(state);
                    None
                }
            }
        })
    }

    /// Retains a state that could not be installed or safely cleaned. The
    /// entry deliberately makes quarantine terminal: a later thread-local
    /// destructor must not execute add-in code after unload.
    pub(crate) fn retain(&self, state: A::LifecycleState) {
        let runtime_address = self.runtime_address();
        LIFECYCLE_STATES.with(|states| {
            let mut states = states.borrow_mut();
            if states
                .iter()
                .any(|entry| entry.runtime_address == runtime_address)
            {
                std::mem::forget(state);
                return;
            }
            states.push(Entry {
                runtime_address,
                state: Some(Box::new(state)),
            });
        });
    }
}
