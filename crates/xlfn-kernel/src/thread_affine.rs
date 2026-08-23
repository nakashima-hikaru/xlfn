//! Thread-affine ownership for lifecycle-local state.
//!
//! The slot is a cross-thread-safe root that never stores `T` itself. The
//! payload lives in thread-local storage, while the slot records which thread
//! owns the current binding. Access is represented by a non-`Send` capability,
//! so lifecycle code must explicitly carry the owner-thread witness.

use std::any::Any;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::{self, ThreadId};

use parking_lot::Mutex;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThreadAffineError {
    WrongThread,
    Unbound,
    StaleAccess,
    MissingValue,
    TypeMismatch,
    Occupied,
    ReentrantAccess,
    TlsUnavailable,
}

#[derive(Debug)]
pub struct ThreadAffineInstallError<T> {
    pub value: T,
    pub reason: ThreadAffineError,
}

impl<T> ThreadAffineInstallError<T> {
    pub fn into_parts(self) -> (T, ThreadAffineError) {
        (self.value, self.reason)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ThreadAffineSlotId(NonZeroUsize);

struct ThreadAffineEntry {
    slot_id: ThreadAffineSlotId,
    value: Option<Box<dyn Any>>,
}

impl Drop for ThreadAffineEntry {
    #[allow(
        clippy::mem_forget,
        reason = "TLS destruction is not an unload proof; leaking is safer than running add-in destruction"
    )]
    fn drop(&mut self) {
        // TLS destruction is not an unload proof. A failed or abandoned
        // lifecycle binding must never run add-in code from a thread-local
        // destructor after the module boundary has become uncertain.
        if let Some(value) = self.value.take() {
            std::mem::forget(value);
        }
    }
}

thread_local! {
    static THREAD_AFFINE_VALUES: RefCell<Vec<ThreadAffineEntry>> =
        const { RefCell::new(Vec::new()) };
}

static NEXT_SLOT_ID: AtomicUsize = AtomicUsize::new(1);

fn allocate_slot_id() -> ThreadAffineSlotId {
    let raw = NEXT_SLOT_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            next.checked_add(1)
        })
        .unwrap_or_else(|_| {
            // Slot IDs are process-local identity. Reusing one after wraparound
            // could make a stale TLS entry address a new runtime generation.
            std::process::abort();
        });
    ThreadAffineSlotId(
        NonZeroUsize::new(raw)
            .expect("thread-affine slot IDs are allocated from a non-zero counter"),
    )
}

fn with_values<R>(
    operation: impl FnOnce(&mut Vec<ThreadAffineEntry>) -> Result<R, ThreadAffineError>,
) -> Result<R, ThreadAffineError> {
    THREAD_AFFINE_VALUES
        .try_with(|values| {
            let mut values = values
                .try_borrow_mut()
                .map_err(|_| ThreadAffineError::ReentrantAccess)?;
            operation(&mut values)
        })
        .map_err(|_| ThreadAffineError::TlsUnavailable)?
}

struct AffinityControl {
    owner: Option<ThreadId>,
    binding: u64,
}

/// A cross-thread-safe root for one thread-affine payload type.
pub struct ThreadAffineSlot<T: 'static> {
    id: OnceLock<ThreadAffineSlotId>,
    affinity: Mutex<AffinityControl>,
    _marker: PhantomData<fn() -> T>,
}

/// A non-copyable witness that the current thread owns one slot binding.
pub struct ThreadAffineAccess<'slot, T: 'static> {
    slot: &'slot ThreadAffineSlot<T>,
    binding: u64,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl<T: 'static> ThreadAffineSlot<T> {
    pub const fn new() -> Self {
        Self {
            id: OnceLock::new(),
            affinity: Mutex::new(AffinityControl {
                owner: None,
                binding: 0,
            }),
            _marker: PhantomData,
        }
    }

    fn id(&self) -> ThreadAffineSlotId {
        *self.id.get_or_init(allocate_slot_id)
    }

    pub fn bind_current(&self) -> Result<ThreadAffineAccess<'_, T>, ThreadAffineError> {
        let current = thread::current().id();
        let mut affinity = self.affinity.lock();
        match affinity.owner {
            Some(owner) if owner != current => return Err(ThreadAffineError::WrongThread),
            Some(_) => {}
            None => {
                affinity.binding = affinity.binding.checked_add(1).unwrap_or_else(|| {
                    std::process::abort();
                });
                affinity.owner = Some(current);
            }
        }
        Ok(ThreadAffineAccess {
            slot: self,
            binding: affinity.binding,
            _not_send_or_sync: PhantomData,
        })
    }

    fn verify_access(&self, access: &ThreadAffineAccess<'_, T>) -> Result<(), ThreadAffineError> {
        if !std::ptr::eq(self, access.slot) {
            return Err(ThreadAffineError::StaleAccess);
        }
        let current = thread::current().id();
        let affinity = self.affinity.lock();
        match affinity.owner {
            None => Err(ThreadAffineError::Unbound),
            Some(owner) if owner != current => Err(ThreadAffineError::WrongThread),
            Some(_) if affinity.binding != access.binding => Err(ThreadAffineError::StaleAccess),
            Some(_) => Ok(()),
        }
    }

    pub fn install(
        &self,
        access: &ThreadAffineAccess<'_, T>,
        value: T,
    ) -> Result<(), ThreadAffineInstallError<T>> {
        if let Err(reason) = self.verify_access(access) {
            return Err(ThreadAffineInstallError { value, reason });
        }

        let slot_id = self.id();
        let mut value = Some(value);
        let result = with_values(|values| {
            if values.iter().any(|entry| entry.slot_id == slot_id) {
                return Err(ThreadAffineError::Occupied);
            }
            values.push(ThreadAffineEntry {
                slot_id,
                value: Some(Box::new(
                    value
                        .take()
                        .expect("thread-affine install value is present"),
                )),
            });
            Ok(())
        });

        match result {
            Ok(()) => Ok(()),
            Err(reason) => Err(ThreadAffineInstallError {
                value: value.expect("failed thread-affine install retains its value"),
                reason,
            }),
        }
    }

    pub fn with_mut<R>(
        &self,
        access: &ThreadAffineAccess<'_, T>,
        operation: impl FnOnce(&mut T) -> R,
    ) -> Result<R, ThreadAffineError> {
        self.verify_access(access)?;
        let slot_id = self.id();
        with_values(|values| {
            let entry = values
                .iter_mut()
                .find(|entry| entry.slot_id == slot_id)
                .ok_or(ThreadAffineError::MissingValue)?;
            let value = entry
                .value
                .as_mut()
                .ok_or(ThreadAffineError::MissingValue)?;
            let value = value
                .downcast_mut::<T>()
                .ok_or(ThreadAffineError::TypeMismatch)?;
            Ok(operation(value))
        })
    }

    pub fn has_value(&self, access: &ThreadAffineAccess<'_, T>) -> Result<bool, ThreadAffineError> {
        self.verify_access(access)?;
        let slot_id = self.id();
        with_values(|values| Ok(values.iter().any(|entry| entry.slot_id == slot_id)))
    }

    pub fn take(&self, access: &ThreadAffineAccess<'_, T>) -> Result<T, ThreadAffineError> {
        self.verify_access(access)?;
        let slot_id = self.id();
        with_values(|values| {
            let index = values
                .iter()
                .position(|entry| entry.slot_id == slot_id)
                .ok_or(ThreadAffineError::MissingValue)?;
            let mut entry = values.swap_remove(index);
            let value = entry.value.take().ok_or(ThreadAffineError::MissingValue)?;
            #[allow(
                clippy::mem_forget,
                reason = "entry.value was extracted; dropping the empty entry wrapper must not invoke TLS fallback cleanup"
            )]
            std::mem::forget(entry);
            match value.downcast::<T>() {
                Ok(value) => Ok(*value),
                Err(value) => {
                    #[allow(
                        clippy::mem_forget,
                        reason = "type mismatch on dynamic downcast; avoid invoking add-in Drop if type is unexpected"
                    )]
                    std::mem::forget(value);
                    Err(ThreadAffineError::TypeMismatch)
                }
            }
        })
    }

    pub fn release_empty_binding(
        &self,
        access: &ThreadAffineAccess<'_, T>,
    ) -> Result<(), ThreadAffineError> {
        self.verify_access(access)?;
        let slot_id = self.id();
        let occupied =
            with_values(|values| Ok(values.iter().any(|entry| entry.slot_id == slot_id)))?;
        if occupied {
            return Err(ThreadAffineError::Occupied);
        }

        let current = thread::current().id();
        let mut affinity = self.affinity.lock();
        match affinity.owner {
            Some(owner) if owner == current && affinity.binding == access.binding => {
                affinity.owner = None;
                Ok(())
            }
            Some(owner) if owner != current => Err(ThreadAffineError::WrongThread),
            Some(_) => Err(ThreadAffineError::StaleAccess),
            None => Err(ThreadAffineError::Unbound),
        }
    }
}

impl<T: 'static> Default for ThreadAffineSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::{assert_impl_all, assert_not_impl_any};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};

    type NonSendState = Rc<()>;

    assert_impl_all!(ThreadAffineSlot<NonSendState>: Send, Sync);
    assert_not_impl_any!(ThreadAffineAccess<'static, NonSendState>: Send, Sync);

    #[derive(Clone, Debug)]
    struct DropProbe {
        drops: Arc<AtomicUsize>,
        dropped_on: Arc<StdMutex<Option<ThreadId>>>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
            *self.dropped_on.lock().expect("drop probe lock") = Some(thread::current().id());
        }
    }

    #[test]
    fn same_thread_install_with_take_and_release() {
        let slot = ThreadAffineSlot::<NonSendState>::new();
        let access = slot.bind_current().unwrap();
        slot.install(&access, Rc::new(())).unwrap();
        slot.with_mut(&access, |state| assert!(Rc::strong_count(state) >= 1))
            .unwrap();
        let _state = slot.take(&access).unwrap();
        slot.release_empty_binding(&access).unwrap();
    }

    #[test]
    fn wrong_thread_cannot_bind_or_drop_value() {
        let slot = Arc::new(ThreadAffineSlot::<DropProbe>::new());
        let drops = Arc::new(AtomicUsize::new(0));
        let dropped_on = Arc::new(StdMutex::new(None));
        let access = slot.bind_current().unwrap();
        slot.install(
            &access,
            DropProbe {
                drops: Arc::clone(&drops),
                dropped_on: Arc::clone(&dropped_on),
            },
        )
        .unwrap();

        let other = Arc::clone(&slot);
        let result = std::thread::spawn(move || other.bind_current().map(|_| ()))
            .join()
            .unwrap();
        assert_eq!(result, Err(ThreadAffineError::WrongThread));
        assert_eq!(drops.load(Ordering::SeqCst), 0);

        let state = slot.take(&access).unwrap();
        drop(state);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(*dropped_on.lock().unwrap(), Some(thread::current().id()));
        slot.release_empty_binding(&access).unwrap();
    }

    #[test]
    fn stale_access_is_rejected_after_binding_reuse() {
        let slot = ThreadAffineSlot::<usize>::new();
        let old = slot.bind_current().unwrap();
        slot.install(&old, 1).unwrap();
        assert_eq!(slot.take(&old).unwrap(), 1);
        slot.release_empty_binding(&old).unwrap();

        let current = slot.bind_current().unwrap();
        assert_eq!(
            slot.with_mut(&old, |_| ()),
            Err(ThreadAffineError::StaleAccess)
        );
        slot.install(&current, 2).unwrap();
        assert_eq!(slot.take(&current).unwrap(), 2);
        slot.release_empty_binding(&current).unwrap();
    }

    #[test]
    fn released_binding_can_move_to_another_thread() {
        let slot = Arc::new(ThreadAffineSlot::<usize>::new());
        let access = slot.bind_current().unwrap();
        slot.install(&access, 1).unwrap();
        assert_eq!(slot.take(&access).unwrap(), 1);
        slot.release_empty_binding(&access).unwrap();

        let other = Arc::clone(&slot);
        std::thread::spawn(move || {
            let access = other.bind_current().unwrap();
            other.install(&access, 2).unwrap();
            assert_eq!(other.take(&access).unwrap(), 2);
            other.release_empty_binding(&access).unwrap();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn duplicate_install_returns_the_second_value() {
        let slot = ThreadAffineSlot::<usize>::new();
        let access = slot.bind_current().unwrap();
        slot.install(&access, 1).unwrap();
        let error = slot.install(&access, 2).unwrap_err();
        assert_eq!(error.reason, ThreadAffineError::Occupied);
        assert_eq!(error.value, 2);
        assert_eq!(slot.take(&access).unwrap(), 1);
        slot.release_empty_binding(&access).unwrap();
    }

    #[test]
    fn tls_destructor_forgets_unclaimed_value() {
        let slot = Arc::new(ThreadAffineSlot::<DropProbe>::new());
        let drops = Arc::new(AtomicUsize::new(0));
        let dropped_on = Arc::new(StdMutex::new(None));
        let thread_slot = Arc::clone(&slot);
        let thread_drops = Arc::clone(&drops);
        let thread_dropped_on = Arc::clone(&dropped_on);
        std::thread::spawn(move || {
            let access = thread_slot.bind_current().unwrap();
            thread_slot
                .install(
                    &access,
                    DropProbe {
                        drops: thread_drops,
                        dropped_on: thread_dropped_on,
                    },
                )
                .unwrap();
        })
        .join()
        .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(dropped_on.lock().unwrap().is_none());
    }
}
