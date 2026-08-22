//! The handle object reclamation safety kernel.
//!
//! This module owns the epoch protocol and the retired-object queue. The
//! surrounding binding/topic code may decide when an object is detached or
//! resurrected, but it cannot redefine the proof that a retired payload is
//! safe to reclaim. Call-scoped pointer witnesses and long-lived pins both
//! use this boundary.
//!
//! This is a frozen safety boundary: new handle lifecycle code must consume
//! the guards exposed here instead of reading epoch counters or retired
//! storage directly. Changes to the reclamation algorithm belong here with
//! its complete safety proof and concurrency tests.

use super::object_access::{BorrowedObject, PinnedObject};
#[cfg(test)]
use super::object_store::ObjectIdentity;
use super::object_store::{
    DetachedObject, ErasedObject, LiveObjectRef, ObjectLocator, ObjectStore,
};
use super::typed::ExcelHandleObject;
use crate::{XllError, XllResult};
use parking_lot::Mutex;
use std::any::TypeId;
use std::cell::OnceCell;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::Weak;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// A payload detached from the live binding table and waiting for the epoch
/// and pin obligations to clear.
pub(super) struct RetiredObject {
    pub(super) epoch: u64,
    pub(super) object: LiveObjectRef,
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

    pub(super) fn release_pin(&mut self, object: LiveObjectRef) {
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
            .position(|entry| {
                entry.pins == 0
                    && entry.object.id == object.id
                    && entry.object.key == object.key_hint
            })
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

// Epochs start at one, so zero is an unambiguous inactive marker. Keeping the
// sentinel outside the live epoch range avoids the `u64::MAX` collision
// between an active reader and the inactive state.
const EPOCH_INACTIVE: u64 = 0;

trait EpochAtomicU64 {
    fn new(value: u64) -> Self;
    fn load(&self, ordering: Ordering) -> u64;
    fn store(&self, value: u64, ordering: Ordering);
}

trait EpochAtomicUsize {
    fn new(value: usize) -> Self;
    fn fetch_add(&self, value: usize, ordering: Ordering) -> usize;
    fn fetch_sub(&self, value: usize, ordering: Ordering) -> usize;
}

impl EpochAtomicU64 for AtomicU64 {
    fn new(value: u64) -> Self {
        AtomicU64::new(value)
    }

    fn load(&self, ordering: Ordering) -> u64 {
        AtomicU64::load(self, ordering)
    }

    fn store(&self, value: u64, ordering: Ordering) {
        AtomicU64::store(self, value, ordering);
    }
}

impl EpochAtomicUsize for AtomicUsize {
    fn new(value: usize) -> Self {
        AtomicUsize::new(value)
    }

    fn fetch_add(&self, value: usize, ordering: Ordering) -> usize {
        AtomicUsize::fetch_add(self, value, ordering)
    }

    fn fetch_sub(&self, value: usize, ordering: Ordering) -> usize {
        AtomicUsize::fetch_sub(self, value, ordering)
    }
}

#[cfg(all(test, not(all(target_os = "windows", target_arch = "x86"))))]
impl EpochAtomicU64 for loom::sync::atomic::AtomicU64 {
    fn new(value: u64) -> Self {
        loom::sync::atomic::AtomicU64::new(value)
    }

    fn load(&self, ordering: Ordering) -> u64 {
        loom::sync::atomic::AtomicU64::load(self, ordering)
    }

    fn store(&self, value: u64, ordering: Ordering) {
        loom::sync::atomic::AtomicU64::store(self, value, ordering);
    }
}

#[cfg(all(test, not(all(target_os = "windows", target_arch = "x86"))))]
impl EpochAtomicUsize for loom::sync::atomic::AtomicUsize {
    fn new(value: usize) -> Self {
        loom::sync::atomic::AtomicUsize::new(value)
    }

    fn fetch_add(&self, value: usize, ordering: Ordering) -> usize {
        loom::sync::atomic::AtomicUsize::fetch_add(self, value, ordering)
    }

    fn fetch_sub(&self, value: usize, ordering: Ordering) -> usize {
        loom::sync::atomic::AtomicUsize::fetch_sub(self, value, ordering)
    }
}

/// The reader-side epoch protocol is generic only over its atomic backend so
/// the production implementation and the Loom model execute the same code.
struct EpochParticipantCore<A, D> {
    announced: A,
    depth: D,
}

impl<A, D> EpochParticipantCore<A, D>
where
    A: EpochAtomicU64,
    D: EpochAtomicUsize,
{
    fn new() -> Self {
        Self {
            announced: A::new(EPOCH_INACTIVE),
            depth: D::new(0),
        }
    }

    fn enter(&self, current: &A) -> u64 {
        let previous_depth = self.depth.fetch_add(1, Ordering::Relaxed);
        if previous_depth == 0 {
            loop {
                let epoch = current.load(Ordering::Acquire);
                self.announced.store(epoch, Ordering::Release);
                if current.load(Ordering::Acquire) == epoch {
                    return epoch;
                }
                self.announced.store(EPOCH_INACTIVE, Ordering::Release);
            }
        }

        self.announced.load(Ordering::Acquire)
    }

    fn leave(&self) {
        let previous_depth = self.depth.fetch_sub(1, Ordering::Release);
        debug_assert!(previous_depth > 0, "handle epoch guard is unbalanced");
        if previous_depth == 1 {
            self.announced.store(EPOCH_INACTIVE, Ordering::Release);
        }
    }

    fn announced(&self, ordering: Ordering) -> u64 {
        self.announced.load(ordering)
    }
}

type EpochParticipant = EpochParticipantCore<AtomicU64, AtomicUsize>;

struct EpochParticipantCacheEntry {
    domain_address: usize,
    domain: Weak<EpochDomain>,
    participant: Arc<EpochParticipant>,
}

thread_local! {
    static EPOCH_PARTICIPANTS: RefCell<Vec<EpochParticipantCacheEntry>> =
        const { RefCell::new(Vec::new()) };
}

/// Per-object-store epoch domain. The global mutex is used only when a
/// thread first joins a domain; warm call entry/exit is TLS plus atomics.
pub(super) struct EpochDomain {
    current: AtomicU64,
    participants: Mutex<Vec<Weak<EpochParticipant>>>,
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
            if let Some(entry) = participants
                .iter()
                .find(|entry| entry.domain_address == address)
                && let Some(domain) = entry.domain.upgrade()
                && Arc::ptr_eq(&domain, self)
            {
                return Arc::clone(&entry.participant);
            }

            let participant = Arc::new(EpochParticipant::new());
            self.participants.lock().push(Arc::downgrade(&participant));
            participants.retain(|entry| entry.domain.upgrade().is_some());
            participants.push(EpochParticipantCacheEntry {
                domain_address: address,
                domain: Arc::downgrade(self),
                participant: Arc::clone(&participant),
            });
            participant
        })
    }

    pub(super) fn enter(self: &Arc<Self>) -> EpochGuard {
        let participant = self.participant();
        participant.enter(&self.current);
        EpochGuard {
            _domain: Arc::clone(self),
            participant,
        }
    }

    pub(super) fn retire_epoch(&self) -> u64 {
        self.current
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                epoch.checked_add(1).filter(|next| *next != EPOCH_INACTIVE)
            })
            .unwrap_or_else(|_| {
                tracing::error!("handle epoch space exhausted; fail-stopping");
                std::process::abort();
            })
    }

    pub(super) fn oldest_active(&self) -> Option<u64> {
        let mut participants = self.participants.lock();
        let mut oldest: Option<u64> = None;
        participants.retain(|participant| {
            let Some(participant) = participant.upgrade() else {
                return false;
            };
            let epoch = participant.announced(Ordering::Acquire);
            if epoch != EPOCH_INACTIVE {
                oldest = Some(oldest.map_or(epoch, |current| current.min(epoch)));
            }
            true
        });
        oldest
    }
}

pub(super) struct EpochGuard {
    _domain: Arc<EpochDomain>,
    participant: Arc<EpochParticipant>,
}

impl Drop for EpochGuard {
    fn drop(&mut self) {
        self.participant.leave();
    }
}

/// A copied raw pointer published with an immutable binding record.
///
/// This value is not a borrow and does not protect the allocation. Resolve it
/// through [`EpochReadGuard`] before dereferencing it.
#[derive(Clone, Copy)]
pub(crate) struct PublishedObjectPtr {
    pub(super) ptr: NonNull<()>,
    pub(super) type_id: TypeId,
    pub(super) type_name: &'static str,
}

// SAFETY: this value is only copied from an `ErasedObject` whose payload is
// `Send + Sync + 'static`. It contains no ownership and is dereferenced only
// after the caller has obtained the corresponding `EpochReadGuard`.
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
        guard: EpochReadGuard<'call>,
    ) -> Option<BorrowedObject<'call, T>> {
        self.typed_ptr::<T>()
            .map(|ptr| BorrowedObject::new(ptr, guard))
    }
}

struct CallRegistration {
    store: Arc<ObjectStore>,
    epoch: EpochGuard,
}

/// The read-side capability that proves an object publication is epoch
/// protected. It has no method that can mutate the object registry.
#[derive(Clone, Copy)]
pub(crate) struct EpochReadGuard<'call> {
    _registration: PhantomData<&'call CallRegistration>,
}

impl<'call> EpochReadGuard<'call> {
    fn new(_registration: &'call CallRegistration) -> Self {
        Self {
            _registration: PhantomData,
        }
    }
}

/// The write-side capability used to promote a borrowed handle to an owned
/// registry pin. It is kept separate from [`EpochReadGuard`] so the read
/// witness cannot accidentally acquire a resurrection or pin operation.
#[derive(Clone, Copy)]
pub(crate) struct PinContext<'call> {
    store: &'call Arc<ObjectStore>,
}

impl PinContext<'_> {
    pub(crate) fn pin<T: ExcelHandleObject>(
        self,
        object: LiveObjectRef,
    ) -> XllResult<PinnedObject<T>> {
        let (pin, ptr) = self.store.pin_or_resurrect::<T>(ObjectLocator {
            id: object.id,
            key_hint: object.key,
        })?;
        Ok(PinnedObject::from_parts(pin, ptr))
    }
}

/// The two call-scoped capabilities produced when a handle call joins an
/// object store. Keeping them as one value ensures both capabilities use the
/// same registration without giving the read witness pin authority.
#[derive(Clone, Copy)]
pub(crate) struct CallHandleCapabilities<'call> {
    read: EpochReadGuard<'call>,
    pin: PinContext<'call>,
}

impl<'call> CallHandleCapabilities<'call> {
    pub(crate) fn read_guard(self) -> EpochReadGuard<'call> {
        self.read
    }

    pub(crate) fn pin_context(self) -> PinContext<'call> {
        self.pin
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
    ) -> XllResult<CallHandleCapabilities<'call>> {
        if let Some(registration) = self.registration.get() {
            if Arc::ptr_eq(&registration.store, store) {
                return Ok(Self::capabilities(registration));
            }
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::HANDLE_CONTEXT,
            });
        }

        let registration = CallRegistration {
            store: Arc::clone(store),
            epoch: store.epoch.enter(),
        };
        if self.registration.set(registration).is_err() {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::HANDLE_CONTEXT,
            });
        }
        Ok(Self::capabilities(
            self.registration
                .get()
                .expect("call object registration was just installed"),
        ))
    }

    fn capabilities<'call>(registration: &'call CallRegistration) -> CallHandleCapabilities<'call> {
        CallHandleCapabilities {
            read: EpochReadGuard::new(registration),
            pin: PinContext {
                store: &registration.store,
            },
        }
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

        let final_domain = Arc::new(EpochDomain::new());
        let guard = final_domain.enter();
        drop(guard);
        EPOCH_PARTICIPANTS.with(|participants| {
            let participants = participants.borrow();
            assert_eq!(participants.len(), 1);
            assert!(participants[0].domain.upgrade().is_some());
        });
        drop(final_domain);
    }

    #[test]
    fn concrete_object_store_reclaims_only_after_epoch_quiescence() {
        let registry = HandleRegistry::new(1);
        let store = Arc::clone(&registry.objects);
        let reader = store.epoch.enter();
        let object = ErasedObject::new(42_u32, Arc::clone(&registry.cleanup));
        let locator = LiveObjectRef {
            id: ObjectIdentity(ObjectId(1)),
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
        let locator = LiveObjectRef {
            id: ObjectIdentity(ObjectId(1)),
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
    fn loom_models_the_concrete_epoch_participant_core() {
        loom::model(|| {
            use loom::sync::Arc;
            use loom::sync::atomic::{AtomicU64, Ordering};
            use loom::thread;

            type LoomParticipant = EpochParticipantCore<AtomicU64, loom::sync::atomic::AtomicUsize>;

            let current = Arc::new(AtomicU64::new(1));
            let participant = Arc::new(LoomParticipant::new());
            let entered = Arc::new(AtomicU64::new(EPOCH_INACTIVE));

            let reader_current = Arc::clone(&current);
            let reader_participant = Arc::clone(&participant);
            let reader_entered = Arc::clone(&entered);
            let reader = thread::spawn(move || {
                let epoch = reader_participant.enter(&reader_current);
                reader_entered.store(epoch, Ordering::Release);
                thread::yield_now();
                reader_participant.leave();
                reader_entered.store(EPOCH_INACTIVE, Ordering::Release);
            });

            let retired_at = current.fetch_add(1, Ordering::AcqRel);
            let active = participant.announced(Ordering::Acquire);
            let can_reclaim = active == EPOCH_INACTIVE || retired_at < active;

            reader.join().expect("loom reader did not finish");
            if can_reclaim {
                let entered_at = entered.load(Ordering::Acquire);
                assert!(
                    entered_at == EPOCH_INACTIVE || entered_at > retired_at,
                    "entered_at={entered_at}, retired_at={retired_at}, active={active}"
                );
            }
        });
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
