//! The handle object reclamation safety kernel.
//!
//! This module owns the epoch protocol and the retired-object queue. The
//! surrounding binding/topic code may decide when an object is detached or
//! resurrected, but it cannot redefine the proof that a retired payload is
//! safe to reclaim. Call-scoped pointer witnesses and long-lived pins both
//! use this boundary.

use super::registry::{DetachedObject, ErasedObject, ObjectLocator, ObjectPin, ObjectStore};
use super::typed::ExcelHandleObject;
use crate::{XllError, XllResult};
use parking_lot::Mutex;
use std::any::TypeId;
use std::cell::OnceCell;
use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// A payload detached from the live binding table and waiting for the epoch
/// and pin obligations to clear.
pub(super) struct RetiredObject {
    pub(super) epoch: u64,
    pub(super) object: ObjectLocator,
    pub(super) pins: usize,
    pub(super) value: ErasedObject,
}

/// Cold-path storage for detached payloads. It deliberately remains a compact
/// vector: identity lookup is only used by alias resurrection and pin release.
pub(super) struct RetiredStore {
    entries: Vec<RetiredObject>,
}

impl RetiredStore {
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub(super) fn retire(
        &mut self,
        mut detached: DetachedObject,
        epoch: u64,
        operation: &'static str,
    ) {
        detached.value.set_drop_operation(operation);
        self.entries.push(RetiredObject {
            epoch,
            object: detached.object,
            pins: detached.pins,
            value: detached.value,
        });
    }

    pub(super) fn retire_all(
        &mut self,
        values: impl IntoIterator<Item = DetachedObject>,
        epoch: u64,
        operation: &'static str,
    ) -> usize {
        let mut count = 0;
        for mut detached in values {
            detached.value.set_drop_operation(operation);
            self.entries.push(RetiredObject {
                epoch,
                object: detached.object,
                pins: detached.pins,
                value: detached.value,
            });
            count += 1;
        }
        count
    }

    pub(super) fn release_pin(&mut self, object: ObjectLocator) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.object == object) {
            debug_assert!(entry.pins > 0);
            if entry.pins > 0 {
                entry.pins -= 1;
            }
        }
    }

    pub(super) fn take_for_resurrection(&mut self, object: ObjectLocator) -> Option<RetiredObject> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.pins == 0 && entry.object == object)
            .or_else(|| {
                self.entries
                    .iter()
                    .position(|entry| entry.pins == 0 && entry.object.id == object.id)
            })?;
        Some(self.entries.swap_remove(index))
    }

    pub(super) fn restore(&mut self, entry: RetiredObject) {
        self.entries.push(entry);
    }

    pub(super) fn reclaim(&mut self, safe_before: u64) -> Vec<ErasedObject> {
        let mut ready = Vec::new();
        let mut pending = Vec::with_capacity(self.entries.len());
        for entry in self.entries.drain(..) {
            if entry.pins == 0 && entry.epoch < safe_before {
                ready.push(entry.value);
            } else {
                pending.push(entry);
            }
        }
        self.entries = pending;
        ready
    }
}

const EPOCH_INACTIVE: u64 = u64::MAX;

struct EpochParticipant {
    announced: AtomicU64,
    depth: AtomicUsize,
}

thread_local! {
    static EPOCH_PARTICIPANTS: RefCell<Vec<(usize, Weak<EpochDomain>, Arc<EpochParticipant>)>> =
        const { RefCell::new(Vec::new()) };
}

/// Per-object-store epoch domain. The global mutex is used only when a
/// thread first joins a domain; warm call entry/exit is TLS plus atomics.
pub(super) struct EpochDomain {
    current: AtomicU64,
    participants: Mutex<Vec<Arc<EpochParticipant>>>,
}

impl EpochDomain {
    pub(super) fn new() -> Self {
        Self {
            current: AtomicU64::new(1),
            participants: Mutex::new(Vec::new()),
        }
    }

    fn participant(self: &Arc<Self>) -> Arc<EpochParticipant> {
        let address = Arc::as_ptr(self).addr();
        EPOCH_PARTICIPANTS.with(|participants| {
            let mut participants = participants.borrow_mut();
            if let Some((_, domain, participant)) = participants
                .iter()
                .find(|(candidate, _, _)| *candidate == address)
                && let Some(domain) = domain.upgrade()
                && Arc::ptr_eq(&domain, self)
            {
                return Arc::clone(participant);
            }

            let participant = Arc::new(EpochParticipant {
                announced: AtomicU64::new(EPOCH_INACTIVE),
                depth: AtomicUsize::new(0),
            });
            self.participants.lock().push(Arc::clone(&participant));
            participants.retain(|(candidate, domain, _)| {
                *candidate != address || domain.upgrade().is_some()
            });
            participants.push((address, Arc::downgrade(self), Arc::clone(&participant)));
            participant
        })
    }

    pub(super) fn enter(self: &Arc<Self>) -> EpochGuard {
        let participant = self.participant();
        let previous_depth = participant.depth.fetch_add(1, Ordering::Relaxed);
        if previous_depth == 0 {
            loop {
                let epoch = self.current.load(Ordering::Acquire);
                participant.announced.store(epoch, Ordering::Release);
                if self.current.load(Ordering::Acquire) == epoch {
                    break;
                }
                participant
                    .announced
                    .store(EPOCH_INACTIVE, Ordering::Release);
            }
        }
        EpochGuard {
            _domain: Arc::clone(self),
            participant,
        }
    }

    pub(super) fn retire_epoch(&self) -> u64 {
        self.current
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                Some(epoch.saturating_add(1))
            })
            .expect("epoch update always succeeds")
    }

    pub(super) fn oldest_active(&self) -> Option<u64> {
        self.participants
            .lock()
            .iter()
            .filter_map(|participant| {
                let epoch = participant.announced.load(Ordering::Acquire);
                (epoch != EPOCH_INACTIVE).then_some(epoch)
            })
            .min()
    }
}

pub(super) struct EpochGuard {
    _domain: Arc<EpochDomain>,
    participant: Arc<EpochParticipant>,
}

impl Drop for EpochGuard {
    fn drop(&mut self) {
        let previous_depth = self.participant.depth.fetch_sub(1, Ordering::Release);
        debug_assert!(previous_depth > 0, "handle epoch guard is unbalanced");
        if previous_depth == 1 {
            self.participant
                .announced
                .store(EPOCH_INACTIVE, Ordering::Release);
        }
    }
}

/// A copied raw pointer published with an immutable binding record.
///
/// This value is not a borrow and does not protect the allocation. Resolve it
/// through [`ObjectReadGuard`] before dereferencing it.
#[derive(Clone, Copy)]
pub(crate) struct PublishedObjectPtr {
    pub(super) ptr: NonNull<()>,
    pub(super) type_id: TypeId,
    pub(super) type_name: &'static str,
}

// SAFETY: this value is only copied from an `ErasedObject` whose payload is
// `Send + Sync + 'static`. It contains no ownership and is dereferenced only
// after the caller has obtained the corresponding `ObjectReadGuard`.
unsafe impl Send for PublishedObjectPtr {}

// SAFETY: same invariant as `Send`; the pointer is exposed only as a shared
// reference after concrete type validation and epoch registration.
unsafe impl Sync for PublishedObjectPtr {}

impl PublishedObjectPtr {
    #[inline]
    pub(crate) fn typed_ptr<T: Send + Sync + 'static>(&self) -> Option<NonNull<T>> {
        (self.type_id == TypeId::of::<T>()).then(|| self.ptr.cast::<T>())
    }

    #[inline]
    pub(crate) fn resolve<'call, T: Send + Sync + 'static>(
        self,
        guard: ObjectReadGuard<'call>,
    ) -> Option<TypedObjectRef<'call, T>> {
        self.typed_ptr::<T>()
            .map(|ptr| TypedObjectRef { ptr, guard })
    }
}

impl ErasedObject {
    #[inline]
    pub(crate) fn published_ptr(&self) -> PublishedObjectPtr {
        PublishedObjectPtr {
            ptr: self.ptr,
            type_id: self.type_id,
            type_name: self.type_name,
        }
    }
}

/// A typed publication pointer whose lifetime is tied to a call's object-read
/// capability. The guard is retained in the value so the pointer cannot be
/// constructed without an epoch witness.
pub(crate) struct TypedObjectRef<'call, T> {
    ptr: NonNull<T>,
    guard: ObjectReadGuard<'call>,
}

impl<T> TypedObjectRef<'_, T> {
    #[inline]
    pub(crate) fn as_ptr(&self) -> NonNull<T> {
        self.ptr
    }

    #[inline]
    pub(crate) fn guard(&self) -> ObjectReadGuard<'_> {
        self.guard
    }
}

struct CallRegistration {
    store: Arc<ObjectStore>,
    epoch: EpochGuard,
}

/// The call-local capability that proves an object publication is epoch
/// protected. It borrows the registration stored by [`HandleCallGuard`], so
/// handles never need to reverse-map a raw store pointer back through the
/// surrounding scope.
#[derive(Clone, Copy)]
pub(crate) struct ObjectReadGuard<'call> {
    registration: &'call CallRegistration,
}

impl<'call> ObjectReadGuard<'call> {
    fn new(registration: &'call CallRegistration) -> Self {
        Self { registration }
    }

    pub(crate) fn pin<T: ExcelHandleObject>(
        self,
        object: ObjectLocator,
    ) -> XllResult<(ObjectPin, NonNull<T>)> {
        self.registration.store.pin_or_resurrect::<T>(object)
    }
}

/// Epoch participation for one Excel callback scope.
pub(crate) struct HandleCallGuard {
    registration: OnceCell<CallRegistration>,
}

impl HandleCallGuard {
    pub(crate) fn new() -> Self {
        Self {
            registration: OnceCell::new(),
        }
    }

    pub(crate) fn register<'call>(
        &'call self,
        store: &Arc<ObjectStore>,
    ) -> XllResult<ObjectReadGuard<'call>> {
        if let Some(registration) = self.registration.get() {
            if Arc::ptr_eq(&registration.store, store) {
                return Ok(ObjectReadGuard::new(registration));
            }
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::HANDLE_CONTEXT,
            });
        }

        let registration = CallRegistration {
            store: Arc::clone(store),
            epoch: store.epoch.enter(),
        };
        if self.registration.set(registration).is_err() {
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::HANDLE_CONTEXT,
            });
        }
        Ok(ObjectReadGuard::new(
            self.registration
                .get()
                .expect("call object registration was just installed"),
        ))
    }
}

impl Drop for HandleCallGuard {
    fn drop(&mut self) {
        if let Some(registration) = self.registration.take() {
            let store = registration.store;
            drop(registration.epoch);
            if store.retired_count.load(Ordering::Relaxed) != 0 {
                store.reclaim();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::{HandleRegistry, ObjectId, ObjectKey};
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn active_reader_delays_epoch_reclamation() {
        let domain = Arc::new(EpochDomain::new());
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let reader_domain = Arc::clone(&domain);
        let reader_entered = Arc::clone(&entered);
        let reader_release = Arc::clone(&release);

        let reader = thread::spawn(move || {
            let outer = reader_domain.enter();
            let inner = reader_domain.enter();
            reader_entered.wait();
            reader_release.wait();
            drop(inner);
            assert_eq!(reader_domain.oldest_active(), Some(1));
            drop(outer);
        });

        entered.wait();
        let retired_at = domain.retire_epoch();
        assert_eq!(retired_at, 1);
        assert_eq!(domain.oldest_active(), Some(1));
        assert!(retired_at >= domain.oldest_active().unwrap());

        release.wait();
        reader.join().expect("epoch reader did not finish");
        assert_eq!(domain.oldest_active(), None);
    }

    #[test]
    fn concrete_epoch_domain_tracks_nested_depth_and_epoch_advance() {
        let domain = Arc::new(EpochDomain::new());
        let outer = domain.enter();
        let inner = domain.enter();

        assert_eq!(domain.oldest_active(), Some(1));
        let retired_at = domain.retire_epoch();
        assert_eq!(retired_at, 1);
        assert_eq!(domain.oldest_active(), Some(1));

        drop(inner);
        assert_eq!(domain.oldest_active(), Some(1));
        drop(outer);
        assert_eq!(domain.oldest_active(), None);

        let next = domain.enter();
        assert_eq!(domain.oldest_active(), Some(2));
        drop(next);
        assert_eq!(domain.oldest_active(), None);
    }

    #[test]
    fn concrete_epoch_domain_tracks_multiple_participants_and_tls_domain_reuse() {
        let domain = Arc::new(EpochDomain::new());
        let entered = Arc::new(Barrier::new(3));
        let release = Arc::new(Barrier::new(3));

        let first_domain = Arc::clone(&domain);
        let first_entered = Arc::clone(&entered);
        let first_release = Arc::clone(&release);
        let first = thread::spawn(move || {
            let guard = first_domain.enter();
            first_entered.wait();
            first_release.wait();
            drop(guard);
        });

        let second_domain = Arc::clone(&domain);
        let second_entered = Arc::clone(&entered);
        let second_release = Arc::clone(&release);
        let second = thread::spawn(move || {
            let guard = second_domain.enter();
            second_entered.wait();
            second_release.wait();
            drop(guard);
        });

        entered.wait();
        assert_eq!(domain.oldest_active(), Some(1));
        assert_eq!(domain.retire_epoch(), 1);
        assert_eq!(domain.oldest_active(), Some(1));
        release.wait();
        first
            .join()
            .expect("first epoch participant did not finish");
        second
            .join()
            .expect("second epoch participant did not finish");
        assert_eq!(domain.oldest_active(), None);

        drop(domain);
        for _ in 0..8 {
            let replacement = Arc::new(EpochDomain::new());
            let guard = replacement.enter();
            assert_eq!(replacement.oldest_active(), Some(1));
            drop(guard);
            assert_eq!(replacement.oldest_active(), None);
        }
    }

    #[test]
    fn concrete_object_store_reclaims_only_after_epoch_quiescence() {
        let registry = HandleRegistry::new(1);
        let store = Arc::clone(&registry.objects);
        let reader = store.epoch.enter();
        let object = ErasedObject::new(42_u32, Arc::clone(&registry.cleanup));
        let locator = ObjectLocator {
            id: ObjectId(1),
            key: ObjectKey {
                namespace: 1,
                slot: 0,
                generation: crate::generation::ObjectGeneration::ONE,
            },
        };

        store.retire(
            DetachedObject {
                object: locator,
                pins: 0,
                value: object,
            },
            "concrete epoch reclamation test",
        );
        store.reclaim();
        assert_eq!(store.retired_count.load(Ordering::Acquire), 1);

        drop(reader);
        store.reclaim();
        assert_eq!(store.retired_count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn retired_store_requires_both_epoch_and_pin_quiescence() {
        let registry = HandleRegistry::new(1);
        let object = ErasedObject::new(42_u32, Arc::clone(&registry.cleanup));
        let locator = ObjectLocator {
            id: ObjectId(1),
            key: ObjectKey {
                namespace: 1,
                slot: 0,
                generation: crate::generation::ObjectGeneration::ONE,
            },
        };
        let mut retired = RetiredStore::new();
        retired.retire(
            DetachedObject {
                object: locator,
                pins: 1,
                value: object,
            },
            1,
            "reclamation test",
        );

        assert!(retired.reclaim(2).is_empty());
        retired.release_pin(locator);
        assert_eq!(retired.reclaim(1).len(), 0);
        assert_eq!(retired.reclaim(2).len(), 1);
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[test]
    fn loom_models_reader_entry_against_retire_and_reclaim() {
        loom::model(|| {
            use loom::sync::Arc;
            use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};
            use loom::thread;

            let current = Arc::new(AtomicU64::new(1));
            let announced = Arc::new(AtomicU64::new(EPOCH_INACTIVE));
            let entered = Arc::new(AtomicU64::new(EPOCH_INACTIVE));
            let reclaimed = Arc::new(AtomicBool::new(false));

            let reader_current = Arc::clone(&current);
            let reader_announced = Arc::clone(&announced);
            let reader_entered = Arc::clone(&entered);
            let reader = thread::spawn(move || {
                let epoch = reader_current.load(Ordering::Acquire);
                reader_announced.store(epoch, Ordering::Release);
                if reader_current.load(Ordering::Acquire) == epoch {
                    reader_entered.store(epoch, Ordering::Release);
                    thread::yield_now();
                    reader_entered.store(EPOCH_INACTIVE, Ordering::Release);
                    reader_announced.store(EPOCH_INACTIVE, Ordering::Release);
                } else {
                    reader_announced.store(EPOCH_INACTIVE, Ordering::Release);
                }
            });

            let retired_at = current.fetch_add(1, Ordering::AcqRel);
            let active = announced.load(Ordering::Acquire);
            let can_reclaim = active == EPOCH_INACTIVE || retired_at < active;
            reclaimed.store(can_reclaim, Ordering::Release);

            reader.join().expect("loom reader did not finish");
            if reclaimed.load(Ordering::Acquire) {
                let entered_at = entered.load(Ordering::Acquire);
                assert!(entered_at == EPOCH_INACTIVE || entered_at > retired_at);
            }
        });
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[test]
    #[ignore = "run in the dedicated Shuttle test step"]
    fn shuttle_models_reader_entry_against_retire_and_reclaim() {
        shuttle::check_random(
            || {
                use shuttle::sync::Arc;
                use shuttle::sync::atomic::{AtomicBool, AtomicU64, Ordering};

                let current = Arc::new(AtomicU64::new(1));
                let announced = Arc::new(AtomicU64::new(EPOCH_INACTIVE));
                let entered = Arc::new(AtomicU64::new(EPOCH_INACTIVE));
                let reclaimed = Arc::new(AtomicBool::new(false));

                let reader_current = Arc::clone(&current);
                let reader_announced = Arc::clone(&announced);
                let reader_entered = Arc::clone(&entered);
                let reader = shuttle::thread::spawn(move || {
                    let epoch = reader_current.load(Ordering::Acquire);
                    reader_announced.store(epoch, Ordering::Release);
                    if reader_current.load(Ordering::Acquire) == epoch {
                        reader_entered.store(epoch, Ordering::Release);
                        shuttle::thread::yield_now();
                        reader_entered.store(EPOCH_INACTIVE, Ordering::Release);
                        reader_announced.store(EPOCH_INACTIVE, Ordering::Release);
                    } else {
                        reader_announced.store(EPOCH_INACTIVE, Ordering::Release);
                    }
                });

                let retired_at = current.fetch_add(1, Ordering::AcqRel);
                let active = announced.load(Ordering::Acquire);
                reclaimed.store(
                    active == EPOCH_INACTIVE || retired_at < active,
                    Ordering::Release,
                );

                reader.join().expect("shuttle reader did not finish");
                if reclaimed.load(Ordering::Acquire) {
                    let entered_at = entered.load(Ordering::Acquire);
                    assert!(entered_at == EPOCH_INACTIVE || entered_at > retired_at);
                }
            },
            100,
        );
    }
}
