//! Sole ownership and liveness of handle payloads.
//!
//! The binding side of the registry stores only [`LiveObjectRef`] values.
//! This module owns the `Box<dyn Any>` payloads, their binding/pin roots, and
//! the transition from live storage to the epoch-retired queue.

use super::reclamation::{EpochDomain, PublishedObjectPtr, RetiredStore};
use super::token::ObjectId;
use super::typed::ExcelHandleObject;
use crate::error::DomainErrorCode;
use crate::generation::ObjectGeneration;
use crate::{XllError, XllResult};
use parking_lot::{Mutex, MutexGuard};
use rustc_hash::FxHashMap;
use std::any::{Any, TypeId, type_name};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

pub(crate) struct HandleCleanupState {
    failure: Mutex<Option<XllError>>,
}

impl HandleCleanupState {
    pub(super) fn new() -> Self {
        Self {
            failure: Mutex::new(None),
        }
    }

    pub(super) fn record(&self, error: XllError) {
        let mut failure = self.failure.lock();
        if failure.is_none() {
            *failure = Some(error);
        }
    }

    pub(super) fn result(&self) -> XllResult<()> {
        self.failure
            .lock()
            .as_ref()
            .map_or(Ok(()), |error| Err(error.clone()))
    }
}

/// A single owner for an object stored in the object registry.
pub(crate) struct ErasedObject {
    owner: Option<Box<dyn Any + Send + Sync>>,
    pub(super) ptr: NonNull<()>,
    pub(super) type_id: TypeId,
    pub(super) type_name: &'static str,
    cleanup: Arc<HandleCleanupState>,
    drop_operation: &'static str,
}

impl ErasedObject {
    pub(crate) fn new<T: Send + Sync + 'static>(
        value: T,
        cleanup: Arc<HandleCleanupState>,
    ) -> Self {
        let owner: Box<dyn Any + Send + Sync> = Box::new(value);
        let ptr = NonNull::from_ref(owner.as_ref()).cast::<()>();
        Self {
            owner: Some(owner),
            ptr,
            type_id: TypeId::of::<T>(),
            type_name: type_name::<T>(),
            cleanup,
            drop_operation: "handle object drop",
        }
    }

    #[inline]
    pub(crate) fn address(&self) -> usize {
        self.ptr.as_ptr().addr()
    }

    pub(super) fn set_drop_operation(&mut self, operation: &'static str) {
        self.drop_operation = operation;
    }
}

// SAFETY: `owner` is the sole allocation owner and is explicitly
// `Send + Sync`; `ptr` is only a stable, non-owning pointer into that owner
// and is never dereferenced without the registry's read/retirement proof.
unsafe impl Send for ErasedObject {}

// SAFETY: same invariant as `Send`; concurrent access to the owner is
// governed by the `T: Send + Sync` bound recorded in the erased owner.
unsafe impl Sync for ErasedObject {}

impl Drop for ErasedObject {
    fn drop(&mut self) {
        let owner = self
            .owner
            .take()
            .expect("erased object owner must be present exactly once");
        let result = catch_unwind(AssertUnwindSafe(|| {
            drop(owner);
        }));
        if result.is_err() {
            let error = XllError::Panic;
            crate::diagnostics::report_no_unwind(self.drop_operation, &error);
            self.cleanup.record(error);
        }
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

/// The live roots that keep an object in the object registry.
///
/// Bindings and long-lived pins are deliberately counted together here so
/// every live-to-retired transition follows one state machine. A detached
/// object carries only its remaining pin count because its binding root has
/// already been removed from this registry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ObjectRoots {
    bindings: usize,
    pins: usize,
}

impl ObjectRoots {
    pub(super) const fn with_binding() -> Self {
        Self {
            bindings: 1,
            pins: 0,
        }
    }

    pub(super) const fn with_pin() -> Self {
        Self {
            bindings: 0,
            pins: 1,
        }
    }

    pub(super) fn add_binding(&mut self) -> XllResult<()> {
        self.bindings = self.bindings.checked_add(1).ok_or(XllError::Domain {
            code: DomainErrorCode::Overflow,
        })?;
        Ok(())
    }

    pub(super) fn add_pin(&mut self) -> XllResult<()> {
        self.pins = self.pins.checked_add(1).ok_or(XllError::Domain {
            code: DomainErrorCode::Overflow,
        })?;
        Ok(())
    }

    pub(super) fn remove_binding(&mut self) -> bool {
        debug_assert!(self.bindings > 0);
        if self.bindings == 0 {
            return false;
        }
        self.bindings -= 1;
        !self.is_rooted()
    }

    pub(super) fn remove_pin(&mut self) -> bool {
        debug_assert!(self.pins > 0);
        if self.pins == 0 {
            return false;
        }
        self.pins -= 1;
        !self.is_rooted()
    }

    pub(super) const fn is_rooted(self) -> bool {
        self.bindings != 0 || self.pins != 0
    }

    pub(super) const fn pins(self) -> usize {
        self.pins
    }
}

/// Generation-checked identity of an object in the runtime-local object
/// registry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ObjectKey {
    pub(crate) namespace: u64,
    pub(crate) slot: u32,
    pub(crate) generation: ObjectGeneration,
}

/// The stable semantic identity of a handle object.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ObjectIdentity(pub(crate) ObjectId);

/// A caller-provided object reference. Its key is only a hint: resurrection
/// may use the identity to recover the retired payload even when the hint is
/// stale.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ObjectLocator {
    pub(crate) id: ObjectIdentity,
    pub(crate) key_hint: ObjectKey,
}

/// A validated live reference. Unlike [`ObjectLocator`], its key is the
/// authoritative storage generation for the current registry entry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct LiveObjectRef {
    pub(crate) id: ObjectIdentity,
    pub(crate) key: ObjectKey,
}

/// Canonical ownership and liveness metadata for one shared handle object.
pub(super) struct ObjectEntry {
    pub(super) object_id: ObjectId,
    pub(super) roots: ObjectRoots,
    pub(super) value: ErasedObject,
}

pub(super) struct DetachedObject {
    pub(super) object: LiveObjectRef,
    pub(super) pins: usize,
    pub(super) value: ErasedObject,
}

struct ObjectSlot {
    generation: ObjectGeneration,
    entry: Option<ObjectEntry>,
}

/// The sole owner of live handle objects.
pub(crate) struct ObjectRegistry {
    namespace: u64,
    slots: Vec<ObjectSlot>,
    free: Vec<usize>,
    by_identity: FxHashMap<ObjectId, ObjectKey>,
}

impl ObjectRegistry {
    pub(super) fn new(namespace: u64) -> Self {
        Self {
            namespace,
            slots: Vec::new(),
            free: Vec::new(),
            by_identity: FxHashMap::default(),
        }
    }

    pub(super) fn key_for_identity(&self, object_id: ObjectId) -> Option<ObjectKey> {
        self.by_identity.get(&object_id).copied()
    }

    pub(super) fn get(&self, key: ObjectKey) -> Option<&ObjectEntry> {
        if key.namespace != self.namespace {
            return None;
        }
        let slot = self.slots.get(key.slot as usize)?;
        (slot.generation == key.generation)
            .then_some(slot.entry.as_ref())
            .flatten()
    }

    pub(super) fn get_mut(&mut self, key: ObjectKey) -> Option<&mut ObjectEntry> {
        if key.namespace != self.namespace {
            return None;
        }
        let slot = self.slots.get_mut(key.slot as usize)?;
        (slot.generation == key.generation)
            .then_some(slot.entry.as_mut())
            .flatten()
    }

    pub(super) fn insert(
        &mut self,
        object_id: ObjectId,
        value: &mut Option<ErasedObject>,
    ) -> XllResult<ObjectKey> {
        self.insert_with_roots(object_id, value, ObjectRoots::with_binding())
    }

    pub(super) fn insert_with_roots(
        &mut self,
        object_id: ObjectId,
        value: &mut Option<ErasedObject>,
        roots: ObjectRoots,
    ) -> XllResult<ObjectKey> {
        debug_assert!(roots.is_rooted());
        let index = self.free.last().copied().unwrap_or(self.slots.len());

        let slot = u32::try_from(index).map_err(|_| XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::HANDLE_SLOT,
        })?;
        if self.free.pop().is_none() {
            self.slots.push(ObjectSlot {
                generation: ObjectGeneration::ONE,
                entry: None,
            });
        }
        let generation = self.slots[index].generation;
        let key = ObjectKey {
            namespace: self.namespace,
            slot,
            generation,
        };
        let entry = ObjectEntry {
            object_id,
            roots,
            value: value.take().expect("object insertion must own its value"),
        };
        debug_assert!(self.slots[index].entry.is_none());
        self.slots[index].entry = Some(entry);
        self.by_identity.insert(object_id, key);
        Ok(key)
    }

    pub(super) fn add_binding(&mut self, object: LiveObjectRef) -> XllResult<()> {
        let entry = self.get_mut(object.key).ok_or(XllError::StaleHandle)?;
        if entry.object_id != object.id.0 {
            return Err(XllError::StaleHandle);
        }
        entry.roots.add_binding()
    }

    pub(super) fn add_pin(&mut self, object: LiveObjectRef) -> XllResult<PublishedObjectPtr> {
        let entry = self.get_mut(object.key).ok_or(XllError::StaleHandle)?;
        if entry.object_id != object.id.0 {
            return Err(XllError::StaleHandle);
        }
        entry.roots.add_pin()?;
        Ok(entry.value.published_ptr())
    }

    pub(super) fn release_binding(&mut self, key: ObjectKey) -> Option<DetachedObject> {
        let index = key.slot as usize;
        let slot = self.slots.get_mut(index)?;
        if slot.generation != key.generation {
            debug_assert!(false, "binding references a stale object key");
            return None;
        }
        let entry = slot.entry.as_mut()?;
        if !entry.roots.remove_binding() {
            return None;
        }

        let entry = slot.entry.take().expect("object entry was checked above");
        self.by_identity.remove(&entry.object_id);
        if let Some(next) = slot.generation.next() {
            slot.generation = next;
            self.free.push(index);
        }
        Some(DetachedObject {
            object: LiveObjectRef {
                id: ObjectIdentity(entry.object_id),
                key,
            },
            pins: entry.roots.pins(),
            value: entry.value,
        })
    }

    pub(super) fn release_pin(&mut self, object: LiveObjectRef) -> Option<DetachedObject> {
        let key = object.key;
        let index = key.slot as usize;
        let slot = self.slots.get_mut(index)?;
        if slot.generation != key.generation {
            debug_assert!(false, "pin references a stale object key");
            return None;
        }
        let entry = slot.entry.as_mut()?;
        if entry.object_id != object.id.0 {
            debug_assert!(false, "pin references a different object identity");
            return None;
        }
        if !entry.roots.remove_pin() {
            return None;
        }

        let entry = slot.entry.take().expect("object entry was checked above");
        self.by_identity.remove(&entry.object_id);
        if let Some(next) = slot.generation.next() {
            slot.generation = next;
            self.free.push(index);
        }
        Some(DetachedObject {
            object: LiveObjectRef {
                id: ObjectIdentity(entry.object_id),
                key,
            },
            pins: entry.roots.pins(),
            value: entry.value,
        })
    }

    pub(super) fn take_all(&mut self) -> Vec<DetachedObject> {
        self.by_identity.clear();
        self.free.clear();
        let mut values = Vec::new();
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(entry) = slot.entry.take() {
                values.push(DetachedObject {
                    object: LiveObjectRef {
                        id: ObjectIdentity(entry.object_id),
                        key: ObjectKey {
                            namespace: self.namespace,
                            slot: u32::try_from(index)
                                .expect("handle object slot must fit in ObjectKey"),
                            generation: slot.generation,
                        },
                    },
                    pins: entry.roots.pins(),
                    value: entry.value,
                });
            }
            if let Some(next) = slot.generation.next() {
                slot.generation = next;
                self.free.push(index);
            }
        }
        values
    }
}

/// Reclamation queue shared with call scopes.
pub(crate) struct ObjectStore {
    pub(super) live: Mutex<ObjectRegistry>,
    retired: Mutex<RetiredStore>,
    pub(super) retired_count: AtomicUsize,
    next_object_id: AtomicU64,
    pub(super) epoch: Arc<EpochDomain>,
    active_pins: AtomicUsize,
    sealed: AtomicBool,
    #[cfg(any(test, feature = "unstable"))]
    ghost: std::sync::OnceLock<crate::shutdown_refinement::GhostHandle>,
}

impl ObjectStore {
    const RECLAIM_THRESHOLD: usize = 64;

    pub(super) fn new(namespace: u64) -> Self {
        Self {
            live: Mutex::new(ObjectRegistry::new(namespace)),
            retired: Mutex::new(RetiredStore::new()),
            retired_count: AtomicUsize::new(0),
            next_object_id: AtomicU64::new(1),
            epoch: Arc::new(EpochDomain::new()),
            active_pins: AtomicUsize::new(0),
            sealed: AtomicBool::new(false),
            #[cfg(any(test, feature = "unstable"))]
            ghost: std::sync::OnceLock::new(),
        }
    }

    #[cfg(any(test, feature = "unstable"))]
    pub(super) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        let _ = self.ghost.set(ghost);
    }

    #[cfg(any(test, feature = "unstable"))]
    pub(super) fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        if let Some(ghost) = self.ghost.get() {
            ghost.record_event(event);
        }
    }

    pub(super) fn lock_live(&self) -> MutexGuard<'_, ObjectRegistry> {
        self.live.lock()
    }

    pub(super) fn seal(&self) {
        let _live = self.live.lock();
        self.sealed.store(true, Ordering::Release);
    }

    pub(super) fn allocate_object_id(&self) -> XllResult<ObjectId> {
        self.next_object_id
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map(ObjectId)
            .map_err(|_| XllError::Domain {
                code: DomainErrorCode::Overflow,
            })
    }

    pub(super) fn retire(&self, detached: DetachedObject, operation: &'static str) {
        let epoch = self.epoch.retire_epoch();
        let mut retired = self.retired.lock();
        retired.retire(detached, epoch, operation);
        self.retired_count.fetch_add(1, Ordering::Relaxed);
        drop(retired);
        self.reclaim_if_needed();
    }

    pub(super) fn retire_all(
        &self,
        values: impl IntoIterator<Item = DetachedObject>,
        operation: &'static str,
    ) {
        let epoch = self.epoch.retire_epoch();
        let mut retired = self.retired.lock();
        let count = retired.retire_all(values, epoch, operation);
        self.retired_count.fetch_add(count, Ordering::Relaxed);
        drop(retired);
        self.reclaim();
    }

    fn reclaim_if_needed(&self) {
        let threshold_reached =
            self.retired_count.load(Ordering::Relaxed) >= Self::RECLAIM_THRESHOLD;
        if threshold_reached || self.epoch.oldest_active().is_none() {
            self.reclaim();
        }
    }

    pub(crate) fn pin_or_resurrect<T>(
        self: &Arc<Self>,
        object: ObjectLocator,
    ) -> XllResult<(ObjectPin, NonNull<T>)>
    where
        T: ExcelHandleObject,
    {
        let object_id = object.id.0;
        let requested_object_key = object.key_hint;
        let mut objects = self.live.lock();
        if self.sealed.load(Ordering::Acquire) {
            return Err(XllError::Closing);
        }
        let live_key = objects
            .get(requested_object_key)
            .filter(|entry| entry.object_id == object_id)
            .map(|_| requested_object_key)
            .or_else(|| objects.key_for_identity(object_id));

        let (object_key, object_ref) = if let Some(live_key) = live_key {
            let entry = objects.get(live_key).ok_or(XllError::StaleHandle)?;
            let object_ref = entry.value.published_ptr();
            if object_ref.typed_ptr::<T>().is_none() {
                let actual_type = entry.value.type_name;
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    tracing::warn!(
                        expected_type = type_name::<T>(),
                        actual_type,
                        "Excel handle pin type mismatch"
                    );
                }));
                return Err(XllError::InvalidHandle);
            }
            let object_ref = objects.add_pin(LiveObjectRef {
                id: ObjectIdentity(object_id),
                key: live_key,
            })?;
            (live_key, object_ref)
        } else {
            let Some((new_key, object_ref)) = self.resurrect(
                &mut objects,
                object,
                TypeId::of::<T>(),
                type_name::<T>(),
                ObjectRoots::with_pin(),
            )?
            else {
                return Err(XllError::StaleHandle);
            };
            // Resurrection installs the pin root directly. There is no
            // temporary binding to add and remove.
            (new_key, object_ref)
        };

        let value = object_ref
            .typed_ptr::<T>()
            .expect("object pin type was validated before installation");

        self.active_pins.fetch_add(1, Ordering::AcqRel);
        #[cfg(any(test, feature = "unstable"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddHandlePin);

        Ok((
            ObjectPin::new(
                Arc::clone(self),
                LiveObjectRef {
                    id: ObjectIdentity(object_id),
                    key: object_key,
                },
            ),
            value,
        ))
    }

    fn release_pin(&self, object: LiveObjectRef) {
        let (was_live, detached) = {
            let mut objects = self.live.lock();
            if objects.get(object.key).is_some() {
                (true, objects.release_pin(object))
            } else {
                (false, None)
            }
        };
        if was_live {
            if let Some(detached) = detached {
                self.retire(detached, "handle pin release");
            }
            self.release_active_pin();
            return;
        }

        self.retired.lock().release_pin(object);
        self.reclaim();
        self.release_active_pin();
    }

    fn release_active_pin(&self) {
        let previous = self.active_pins.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "handle pin accounting is unbalanced");
    }

    pub(super) fn finish_quiescence(&self) -> XllResult<()> {
        if self.active_pins.load(Ordering::Acquire) != 0 {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::HANDLE_PINS,
            });
        }
        self.reclaim();
        Ok(())
    }

    pub(super) fn resurrect(
        &self,
        objects: &mut ObjectRegistry,
        object: ObjectLocator,
        expected_type: TypeId,
        expected_type_name: &'static str,
        roots: ObjectRoots,
    ) -> XllResult<Option<(ObjectKey, PublishedObjectPtr)>> {
        let mut retired = self.retired.lock();
        let Some(mut detached) = retired.take_for_resurrection(object) else {
            return Ok(None);
        };
        self.retired_count.fetch_sub(1, Ordering::Relaxed);

        if detached.value.type_id != expected_type {
            let actual_type = detached.value.type_name;
            retired.restore(detached);
            self.retired_count.fetch_add(1, Ordering::Relaxed);
            let _ = catch_unwind(AssertUnwindSafe(|| {
                tracing::warn!(
                    expected_type = expected_type_name,
                    actual_type,
                    "Excel handle alias type mismatch"
                );
            }));
            return Err(XllError::InvalidHandle);
        }

        let mut value = Some(detached.value);
        let result = objects.insert_with_roots(object.id.0, &mut value, roots);
        let new_key = match result {
            Ok(new_key) => new_key,
            Err(error) => {
                detached.value = value.take().expect("failed resurrection retains its value");
                retired.restore(detached);
                self.retired_count.fetch_add(1, Ordering::Relaxed);
                return Err(error);
            }
        };
        let object_ref = objects
            .get(new_key)
            .expect("resurrected object must be present")
            .value
            .published_ptr();
        Ok(Some((new_key, object_ref)))
    }

    pub(super) fn reclaim(&self) {
        let safe_before = self.epoch.oldest_active().unwrap_or(u64::MAX);
        let ready = {
            let mut retired = self.retired.lock();
            let ready = retired.reclaim(safe_before);
            self.retired_count.fetch_sub(ready.len(), Ordering::Relaxed);
            ready
        };
        drop(ready);
    }
}

/// An owned registry pin used by long-lived handle types.
pub(crate) struct ObjectPin {
    store: Arc<ObjectStore>,
    object: LiveObjectRef,
}

impl ObjectPin {
    pub(super) fn new(store: Arc<ObjectStore>, object: LiveObjectRef) -> Self {
        Self { store, object }
    }

    pub(crate) fn object_id(&self) -> ObjectId {
        self.object.id.0
    }
}

impl Drop for ObjectPin {
    fn drop(&mut self) {
        self.store.release_pin(self.object);
        #[cfg(any(test, feature = "unstable"))]
        self.store
            .record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandlePin);
    }
}
