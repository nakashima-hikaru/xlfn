use super::reclamation::{EpochDomain, ObjectReadGuard, PublishedObjectPtr, RetiredStore};
use super::{ExcelHandleObject, Handle, HandleId, HandleToken, ObjectId, TokenCodec};
use crate::error::DomainErrorCode;
use crate::generation::{BindingGeneration, ObjectGeneration};
use crate::{XllError, XllResult};
use arc_swap::ArcSwapAny;
use parking_lot::{Mutex, RwLock, RwLockWriteGuard};
use rustc_hash::FxHashMap;
use std::any::{Any, TypeId, type_name};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BindingState {
    Live = 0,
    Retired = 1,
}

impl BindingState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            value if value == Self::Live as u8 => Self::Live,
            value if value == Self::Retired as u8 => Self::Retired,
            _ => Self::Retired,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandleRegistryPhase {
    Open = 0,
    Closing = 1,
    Closed = 2,
}

impl HandleRegistryPhase {
    fn from_raw(raw: u8) -> Self {
        match raw {
            value if value == Self::Open as u8 => Self::Open,
            value if value == Self::Closing as u8 => Self::Closing,
            value if value == Self::Closed as u8 => Self::Closed,
            _ => Self::Closed,
        }
    }
}

pub(crate) struct HandleCleanupState {
    failure: Mutex<Option<XllError>>,
}

#[derive(Debug)]
pub(crate) struct HandleRegistrySealed {
    _private: (),
}

impl HandleRegistrySealed {
    fn new() -> Self {
        Self { _private: () }
    }
}

impl HandleCleanupState {
    fn new() -> Self {
        Self {
            failure: Mutex::new(None),
        }
    }

    fn record(&self, error: XllError) {
        let mut failure = self.failure.lock();
        if failure.is_none() {
            *failure = Some(error);
        }
    }

    fn result(&self) -> XllResult<()> {
        self.failure
            .lock()
            .as_ref()
            .map_or(Ok(()), |error| Err(error.clone()))
    }
}

/// A single owner for an object stored in the object registry.
///
/// This is deliberately a type-erased `Box<dyn Any + Send + Sync>`, not an
/// `Arc<T>`. The registry
/// owns the value for as long as at least one binding refers to its
/// [`ObjectKey`]. Call-scoped handles only borrow the payload while the epoch
/// guard is active.
///
/// # Invariants
///
/// - `owner` contains exactly one concrete `T: Send + Sync + 'static`.
/// - `type_id == TypeId::of::<T>()`.
/// - The owner is moved exactly once into the object registry or retired
///   queue.
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
        let ptr = NonNull::from(owner.as_ref()).cast::<()>();
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

/// One canonical formula-binding record shared by the mutable registry and
/// the immutable read-side publication snapshot.
///
/// Immutable ArcSwap snapshots publish binding metadata and a non-owning raw
/// object pointer. Object reclamation is governed by the call epoch, so an
/// old snapshot does not own or extend the lifetime of the object payload.
pub(crate) struct BindingRecord {
    pub(crate) id: HandleId,
    pub(crate) object: LiveObjectRef,
    pub(crate) object_ref: PublishedObjectPtr,
    pub(crate) state: AtomicU8,
}

impl BindingRecord {
    fn new(id: HandleId, object: LiveObjectRef, object_ref: PublishedObjectPtr) -> Self {
        Self {
            id,
            object,
            object_ref,
            state: AtomicU8::new(BindingState::Live as u8),
        }
    }

    pub(crate) fn state(&self) -> BindingState {
        BindingState::from_raw(self.state.load(Ordering::Acquire))
    }
}

const BINDING_CHUNK_SIZE: usize = 64;

#[derive(Clone)]
struct BindingChunk {
    entries: [Option<triomphe::Arc<BindingRecord>>; BINDING_CHUNK_SIZE],
}

impl BindingChunk {
    fn empty() -> Self {
        Self {
            entries: [const { None }; BINDING_CHUNK_SIZE],
        }
    }
}

pub(crate) struct BindingSnapshot {
    guard: arc_swap::Guard<triomphe::Arc<BindingChunk>>,
}

impl BindingSnapshot {
    pub(crate) fn get(&self, slot: u32) -> Option<&triomphe::Arc<BindingRecord>> {
        self.guard.entries[slot as usize & (BINDING_CHUNK_SIZE - 1)].as_ref()
    }
}

/// Immutable slot-indexed publication snapshots for warm handle lookup.
///
/// Slots are allocated from a bounded registry, so a hash table adds hashing
/// and probing without adding lookup information. Copy-on-write is limited to
/// one 64-entry chunk on insert/remove; readers perform only chunk selection
/// and indexed access.
pub(crate) struct PublishedBindings {
    chunks: Box<[ArcSwapAny<triomphe::Arc<BindingChunk>>]>,
    empty: ArcSwapAny<triomphe::Arc<BindingChunk>>,
}

impl PublishedBindings {
    pub(crate) fn new(maximum_bindings: u32) -> Self {
        let chunk_count = (maximum_bindings as usize)
            .div_ceil(BINDING_CHUNK_SIZE)
            .max(1);
        let empty_chunk = triomphe::Arc::new(BindingChunk::empty());
        Self {
            chunks: (0..chunk_count)
                .map(|_| ArcSwapAny::new(triomphe::Arc::clone(&empty_chunk)))
                .collect(),
            empty: ArcSwapAny::new(empty_chunk),
        }
    }

    fn chunk_index(slot: u32) -> usize {
        slot as usize / BINDING_CHUNK_SIZE
    }

    /// Load the chunk containing one publication.
    ///
    /// The guard must remain alive while the caller validates and uses the
    /// borrowed publication. This avoids cloning the publication's `Arc` on
    /// every warm lookup while still keeping the immutable snapshot alive.
    pub(crate) fn load(&self, slot: u32) -> BindingSnapshot {
        let chunk = self
            .chunks
            .get(Self::chunk_index(slot))
            .unwrap_or(&self.empty);
        BindingSnapshot {
            guard: chunk.load(),
        }
    }

    /// Update the snapshot while the canonical registry write lock is held.
    fn insert(&self, id: HandleId, record: triomphe::Arc<BindingRecord>) {
        let slot = id.slot;
        let Some(chunk) = self.chunks.get(Self::chunk_index(slot)) else {
            debug_assert!(false, "handle slot exceeds the publication table");
            return;
        };
        let current = chunk.load_full();
        let mut next = current.as_ref().clone();
        next.entries[slot as usize & (BINDING_CHUNK_SIZE - 1)] = Some(record);
        chunk.store(triomphe::Arc::new(next));
    }

    /// Remove only the publication that belongs to the canonical entry being
    /// removed. The identity check keeps a future slot reuse from removing a
    /// newer generation if this helper is ever called outside the current
    /// write-lock discipline.
    fn remove(&self, id: HandleId, expected: &triomphe::Arc<BindingRecord>) {
        let slot = id.slot;
        let Some(chunk) = self.chunks.get(Self::chunk_index(slot)) else {
            return;
        };
        let current = chunk.load_full();
        if !current.entries[slot as usize & (BINDING_CHUNK_SIZE - 1)]
            .as_ref()
            .is_some_and(|record| triomphe::Arc::ptr_eq(record, expected))
        {
            return;
        }
        let mut next = current.as_ref().clone();
        next.entries[slot as usize & (BINDING_CHUNK_SIZE - 1)] = None;
        chunk.store(triomphe::Arc::new(next));
    }

    /// Clear all publication snapshots while the canonical registry is being
    /// closed.
    fn clear(&self) {
        let empty_chunk = triomphe::Arc::new(BindingChunk::empty());
        for chunk in &self.chunks {
            chunk.store(triomphe::Arc::clone(&empty_chunk));
        }
    }
}

pub(crate) struct BindingSlot {
    pub(crate) next_generation: BindingGeneration,
    pub(crate) record: Option<triomphe::Arc<BindingRecord>>,
}

/// Generation-checked identity of an object in the runtime-local object
/// registry. It is intentionally separate from [`ObjectId`]: `ObjectId` is
/// the stable semantic identity used by formula revisions, while this key is
/// the current storage identity used to prevent ABA after object retirement.
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

/// Canonical ownership and binding-count metadata for one shared handle
/// object. Type metadata remains in [`ErasedObject`], the sole owner of the
/// payload, so it cannot diverge from the drop function.
pub(crate) struct ObjectEntry {
    pub(crate) object_id: ObjectId,
    pub(crate) bindings: usize,
    pub(crate) pins: usize,
    pub(crate) value: ErasedObject,
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
///
/// Binding records and immutable publication snapshots contain an [`ObjectKey`]
/// plus a copied, non-owning [`PublishedObjectPtr`]. The registry remains the
/// sole owner of each payload; readers resolve its typed pointer through an
/// [`ObjectReadGuard`] for the duration of a call epoch.
pub(crate) struct ObjectRegistry {
    namespace: u64,
    slots: Vec<ObjectSlot>,
    free: Vec<usize>,
    by_identity: FxHashMap<ObjectId, ObjectKey>,
}

impl ObjectRegistry {
    fn new(namespace: u64) -> Self {
        Self {
            namespace,
            slots: Vec::new(),
            free: Vec::new(),
            by_identity: FxHashMap::default(),
        }
    }

    fn key_for_identity(&self, object_id: ObjectId) -> Option<ObjectKey> {
        self.by_identity.get(&object_id).copied()
    }

    fn get(&self, key: ObjectKey) -> Option<&ObjectEntry> {
        if key.namespace != self.namespace {
            return None;
        }
        let slot = self.slots.get(key.slot as usize)?;
        (slot.generation == key.generation)
            .then_some(slot.entry.as_ref())
            .flatten()
    }

    fn get_mut(&mut self, key: ObjectKey) -> Option<&mut ObjectEntry> {
        if key.namespace != self.namespace {
            return None;
        }
        let slot = self.slots.get_mut(key.slot as usize)?;
        (slot.generation == key.generation)
            .then_some(slot.entry.as_mut())
            .flatten()
    }

    fn insert(
        &mut self,
        object_id: ObjectId,
        value: &mut Option<ErasedObject>,
    ) -> XllResult<ObjectKey> {
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
            bindings: 1,
            pins: 0,
            value: value.take().expect("object insertion must own its value"),
        };
        debug_assert!(self.slots[index].entry.is_none());
        self.slots[index].entry = Some(entry);
        self.by_identity.insert(object_id, key);
        Ok(key)
    }

    fn add_binding(&mut self, object: LiveObjectRef) -> XllResult<()> {
        let entry = self.get_mut(object.key).ok_or(XllError::StaleHandle)?;
        if entry.object_id != object.id.0 {
            return Err(XllError::StaleHandle);
        }
        entry.bindings = entry.bindings.checked_add(1).ok_or(XllError::Domain {
            code: DomainErrorCode::Overflow,
        })?;
        Ok(())
    }

    fn add_pin(&mut self, object: LiveObjectRef) -> XllResult<PublishedObjectPtr> {
        let entry = self.get_mut(object.key).ok_or(XllError::StaleHandle)?;
        if entry.object_id != object.id.0 {
            return Err(XllError::StaleHandle);
        }
        entry.pins = entry.pins.checked_add(1).ok_or(XllError::Domain {
            code: DomainErrorCode::Overflow,
        })?;
        Ok(entry.value.published_ptr())
    }

    fn release_binding(&mut self, key: ObjectKey) -> Option<DetachedObject> {
        let index = key.slot as usize;
        let slot = self.slots.get_mut(index)?;
        if slot.generation != key.generation {
            debug_assert!(false, "binding references a stale object key");
            return None;
        }
        let entry = slot.entry.as_mut()?;
        debug_assert!(entry.bindings > 0);
        entry.bindings -= 1;
        if entry.bindings != 0 || entry.pins != 0 {
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
            pins: 0,
            value: entry.value,
        })
    }

    fn release_pin(&mut self, object: LiveObjectRef) -> Option<DetachedObject> {
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
        debug_assert!(entry.pins > 0);
        if entry.pins == 0 {
            return None;
        }
        entry.pins -= 1;
        if entry.bindings != 0 || entry.pins != 0 {
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
            pins: 0,
            value: entry.value,
        })
    }

    fn take_all(&mut self) -> Vec<DetachedObject> {
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
                    pins: entry.pins,
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

/// Reclamation queue shared with call scopes. A runtime-specific registration
/// is created only when a scope resolves a handle, so scalar Excel calls do
/// not acquire an epoch or touch the queue.
pub(crate) struct ObjectStore {
    live: Mutex<ObjectRegistry>,
    retired: Mutex<RetiredStore>,
    pub(super) retired_count: AtomicUsize,
    next_object_id: AtomicU64,
    pub(super) epoch: Arc<EpochDomain>,
    active_pins: AtomicUsize,
    sealed: AtomicBool,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: std::sync::OnceLock<crate::shutdown_refinement::GhostHandle>,
}

impl ObjectStore {
    const RECLAIM_THRESHOLD: usize = 64;

    fn new(namespace: u64) -> Self {
        Self {
            live: Mutex::new(ObjectRegistry::new(namespace)),
            retired: Mutex::new(RetiredStore::new()),
            retired_count: AtomicUsize::new(0),
            next_object_id: AtomicU64::new(1),
            epoch: Arc::new(EpochDomain::new()),
            active_pins: AtomicUsize::new(0),
            sealed: AtomicBool::new(false),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: std::sync::OnceLock::new(),
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        let _ = self.ghost.set(ghost);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        if let Some(ghost) = self.ghost.get() {
            ghost.record_event(event);
        }
    }

    fn seal(&self) {
        let _live = self.live.lock();
        self.sealed.store(true, Ordering::Release);
    }

    fn allocate_object_id(&self) -> XllResult<ObjectId> {
        self.next_object_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
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

    fn retire_all(
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
            let Some((new_key, _)) =
                self.resurrect(&mut objects, object, TypeId::of::<T>(), type_name::<T>())?
            else {
                return Err(XllError::StaleHandle);
            };
            let object_ref = objects.add_pin(LiveObjectRef {
                id: ObjectIdentity(object_id),
                key: new_key,
            })?;
            // `resurrect` uses the normal binding insertion path, so remove
            // that temporary binding after installing the real pin. Keeping
            // the pin first prevents the payload from detaching between the
            // two operations.
            let temporary_binding = objects.release_binding(new_key);
            debug_assert!(temporary_binding.is_none());
            (new_key, object_ref)
        };

        let value = object_ref
            .typed_ptr::<T>()
            .expect("object pin type was validated before installation");

        self.active_pins.fetch_add(1, Ordering::AcqRel);
        #[cfg(any(test, feature = "shutdown-refinement"))]
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

    fn finish_quiescence(&self) -> XllResult<()> {
        if self.active_pins.load(Ordering::Acquire) != 0 {
            return Err(XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::HANDLE_PINS,
            });
        }
        self.reclaim();
        Ok(())
    }

    fn resurrect(
        &self,
        objects: &mut ObjectRegistry,
        object: ObjectLocator,
        expected_type: TypeId,
        expected_type_name: &'static str,
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
        let result = objects.insert(object.id.0, &mut value);
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
///
/// The pin keeps the registry payload alive without putting an `Arc<T>` around
/// the application object. It may outlive the handle runtime itself; terminal
/// close moves pinned payloads to the retired object store and the final pin drop
/// releases them there.
pub(crate) struct ObjectPin {
    store: Arc<ObjectStore>,
    object: LiveObjectRef,
}

impl ObjectPin {
    fn new(store: Arc<ObjectStore>, object: LiveObjectRef) -> Self {
        Self { store, object }
    }

    pub(crate) fn object_id(&self) -> ObjectId {
        self.object.id.0
    }
}

impl Drop for ObjectPin {
    fn drop(&mut self) {
        self.store.release_pin(self.object);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.store
            .record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandlePin);
    }
}

pub(crate) struct RegistryState {
    pub(crate) slots: Vec<BindingSlot>,
    pub(crate) free: Vec<usize>,
    pub(crate) live_bindings: u32,
}

/// Canonical binding ownership and its immutable read-side publication.
/// Object lifetime is deliberately not part of this type; it is owned by
/// [`ObjectStore`] and referenced through [`LiveObjectRef`] in each record.
pub(crate) struct BindingTable {
    state: RwLock<RegistryState>,
    published: PublishedBindings,
    maximum_bindings: u32,
}

impl BindingTable {
    fn new(maximum_bindings: u32) -> Self {
        Self {
            state: RwLock::new(RegistryState {
                slots: Vec::new(),
                free: Vec::new(),
                live_bindings: 0,
            }),
            published: PublishedBindings::new(maximum_bindings),
            maximum_bindings,
        }
    }

    fn reserve(&self) -> XllResult<BindingReservation<'_>> {
        let mut state = self.state.write();
        if state.live_bindings >= self.maximum_bindings {
            return Err(XllError::Domain {
                code: DomainErrorCode::Overflow,
            });
        }

        let (index, slot, reused, appended) = match state.free.pop() {
            Some(index) => {
                let slot = match u32::try_from(index) {
                    Ok(slot) => slot,
                    Err(_) => {
                        state.free.push(index);
                        return Err(XllError::Internal {
                            diagnostic_id: crate::error::DiagnosticId::HANDLE_SLOT,
                        });
                    }
                };
                (index, slot, true, false)
            }
            None => {
                let index = state.slots.len();
                let slot = u32::try_from(index).map_err(|_| XllError::Domain {
                    code: DomainErrorCode::Overflow,
                })?;
                state.slots.push(BindingSlot {
                    next_generation: BindingGeneration::ONE,
                    record: None,
                });
                (index, slot, false, true)
            }
        };

        let id = HandleId {
            slot,
            generation: state.slots[index].next_generation,
        };
        Ok(BindingReservation {
            table: self,
            state: Some(state),
            index,
            id,
            reused,
            appended,
            active: true,
        })
    }

    pub(crate) fn read_state(&self) -> parking_lot::RwLockReadGuard<'_, RegistryState> {
        self.state.read()
    }

    #[cfg(test)]
    pub(crate) fn write_state(&self) -> parking_lot::RwLockWriteGuard<'_, RegistryState> {
        self.state.write()
    }

    pub(crate) fn published(&self) -> &PublishedBindings {
        &self.published
    }

    fn begin_removal(&self, id: HandleId) -> XllResult<BindingRemoval<'_>> {
        let state = self.state.write();
        let record = state
            .slots
            .get(id.slot as usize)
            .and_then(|slot| slot.record.as_ref())
            .filter(|record| record.id == id)
            .cloned()
            .ok_or(XllError::StaleHandle)?;
        Ok(BindingRemoval {
            table: self,
            state: Some(state),
            id,
            object: record.object,
            record,
            active: true,
        })
    }

    fn retire_all(&self) -> u32 {
        let mut state = self.state.write();
        let live_bindings = state.live_bindings;
        state.free.clear();
        self.published.clear();
        for index in 0..state.slots.len() {
            let reusable = {
                let slot = &mut state.slots[index];
                if let Some(record) = slot.record.take() {
                    record
                        .state
                        .store(BindingState::Retired as u8, Ordering::Release);
                    drop(record);
                }
                if let Some(next) = slot.next_generation.next() {
                    slot.next_generation = next;
                    true
                } else {
                    false
                }
            };
            if reusable {
                state.free.push(index);
            }
        }
        state.live_bindings = 0;
        live_bindings
    }
}

pub(crate) struct BindingReservation<'table> {
    table: &'table BindingTable,
    state: Option<RwLockWriteGuard<'table, RegistryState>>,
    index: usize,
    id: HandleId,
    reused: bool,
    appended: bool,
    active: bool,
}

impl BindingReservation<'_> {
    fn publish(
        mut self,
        object: LiveObjectRef,
        object_ref: PublishedObjectPtr,
    ) -> (HandleId, bool) {
        let mut state = self
            .state
            .take()
            .expect("binding reservation must own the table write lock");
        let record = triomphe::Arc::new(BindingRecord::new(self.id, object, object_ref));
        state.slots[self.index].record = Some(triomphe::Arc::clone(&record));
        self.table.published.insert(self.id, record);
        state.live_bindings = state
            .live_bindings
            .checked_add(1)
            .expect("binding reservation capacity was checked before commit");
        self.active = false;
        drop(state);
        (self.id, self.reused)
    }
}

impl Drop for BindingReservation<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .state
            .take()
            .expect("binding reservation must own the table write lock");
        if self.appended {
            let slot = state
                .slots
                .pop()
                .expect("new binding reservation owns the final slot");
            debug_assert!(slot.record.is_none());
        } else if self.reused {
            state.free.push(self.index);
        }
    }
}

pub(crate) struct BindingRemoval<'table> {
    table: &'table BindingTable,
    state: Option<RwLockWriteGuard<'table, RegistryState>>,
    id: HandleId,
    object: LiveObjectRef,
    record: triomphe::Arc<BindingRecord>,
    active: bool,
}

impl BindingRemoval<'_> {
    fn object(&self) -> LiveObjectRef {
        self.object
    }

    fn commit(mut self) -> bool {
        let mut state = self
            .state
            .take()
            .expect("binding removal must own the table write lock");
        self.record
            .state
            .store(BindingState::Retired as u8, Ordering::Release);
        self.table.published.remove(self.id, &self.record);
        let slot = state
            .slots
            .get_mut(self.id.slot as usize)
            .expect("binding slot was checked above");
        let slot_record = slot
            .record
            .take()
            .expect("binding record was checked above");
        drop(slot_record);
        let reusable = if let Some(next) = slot.next_generation.next() {
            slot.next_generation = next;
            true
        } else {
            false
        };
        state.live_bindings = state
            .live_bindings
            .checked_sub(1)
            .expect("binding removal cannot underflow live count");
        if reusable {
            state.free.push(self.id.slot as usize);
        }
        self.active = false;
        drop(state);
        reusable
    }
}

impl Drop for BindingRemoval<'_> {
    fn drop(&mut self) {
        debug_assert_eq!(self.state.is_some(), self.active);
    }
}

/// Write-side registry transaction.
///
/// The binding reservation and the live-object arena lock are acquired in the
/// canonical order and are kept together until publication. This makes the
/// object binding count, storage generation, and immutable binding snapshot a
/// single internal mutation boundary without changing the lock-free read path.
pub(crate) struct RegistryWriteTxn<'a> {
    objects: parking_lot::MutexGuard<'a, ObjectRegistry>,
    binding: BindingReservation<'a>,
}

impl<'a> RegistryWriteTxn<'a> {
    fn reserve(registry: &'a HandleRegistry) -> XllResult<Self> {
        let binding = registry.bindings.reserve()?;
        let objects = registry.objects.live.lock();
        Ok(Self { objects, binding })
    }

    fn objects(&mut self) -> &mut ObjectRegistry {
        &mut self.objects
    }

    fn publish(self, object: LiveObjectRef, object_ref: PublishedObjectPtr) -> (HandleId, bool) {
        let Self { objects, binding } = self;
        drop(objects);
        binding.publish(object, object_ref)
    }
}

/// Write-side removal transaction. It mirrors [`RegistryWriteTxn`] for the
/// retirement path and keeps the binding record and object entry paired until
/// both mutations have been linearized.
pub(crate) struct RegistryRemovalTxn<'a> {
    objects: parking_lot::MutexGuard<'a, ObjectRegistry>,
    binding: BindingRemoval<'a>,
}

impl<'a> RegistryRemovalTxn<'a> {
    fn begin(registry: &'a HandleRegistry, id: HandleId) -> XllResult<Self> {
        let binding = registry.bindings.begin_removal(id)?;
        let objects = registry.objects.live.lock();
        Ok(Self { objects, binding })
    }

    fn object(&self) -> LiveObjectRef {
        self.binding.object()
    }

    fn objects(&mut self) -> &mut ObjectRegistry {
        &mut self.objects
    }

    fn commit(self) -> bool {
        let Self { objects, binding } = self;
        drop(objects);
        binding.commit()
    }
}

pub(crate) struct HandleRegistry {
    pub(crate) codec: TokenCodec,
    pub(crate) phase: AtomicU8,
    pub(crate) bindings: BindingTable,
    pub(crate) cleanup: Arc<HandleCleanupState>,
    pub(crate) objects: Arc<ObjectStore>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
}

pub(crate) struct PendingHandleValue<'a> {
    registry: &'a HandleRegistry,
    value: Option<ErasedObject>,
    operation: &'static str,
}

impl<'a> PendingHandleValue<'a> {
    pub(crate) fn new(
        registry: &'a HandleRegistry,
        value: ErasedObject,
        operation: &'static str,
    ) -> Self {
        Self {
            registry,
            value: Some(value),
            operation,
        }
    }

    pub(crate) fn slot(&mut self) -> &mut Option<ErasedObject> {
        &mut self.value
    }
}

impl Drop for PendingHandleValue<'_> {
    fn drop(&mut self) {
        if let Some(mut value) = self.value.take() {
            value.set_drop_operation(self.operation);
            drop(value);
            self.registry.objects.reclaim();
        }
    }
}

impl HandleRegistry {
    pub fn try_new(maximum_bindings: usize) -> XllResult<Self> {
        Self::try_new_with(maximum_bindings, |entropy| getrandom::fill(entropy), true)
    }

    pub(crate) fn try_new_with<E>(
        maximum_bindings: usize,
        fill: impl FnOnce(&mut [u8; 40]) -> Result<(), E>,
        report_failure: bool,
    ) -> XllResult<Self>
    where
        E: std::fmt::Debug,
    {
        let maximum_bindings = u32::try_from(maximum_bindings).map_err(|_| XllError::Domain {
            code: DomainErrorCode::Overflow,
        })?;
        let mut entropy = [0_u8; 40];
        if let Err(source) = fill(&mut entropy) {
            let error = XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::HANDLE_ENTROPY,
            };
            if report_failure {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    tracing::error!(
                        error = ?source,
                        diagnostic_id = crate::error::DiagnosticId::HANDLE_ENTROPY.as_u64(),
                        "OS CSPRNG failed while initializing Excel handle tokens"
                    );
                }));
                crate::diagnostics::report_no_unwind("handle_registry_init", &error);
            }
            return Err(error);
        }
        Ok(Self::from_entropy(maximum_bindings, entropy))
    }

    pub(crate) fn from_entropy(maximum_bindings: u32, entropy: [u8; 40]) -> Self {
        let session = u64::from_le_bytes(
            entropy[..8]
                .try_into()
                .expect("the session entropy slice has eight bytes"),
        );
        let secret = entropy[8..]
            .try_into()
            .expect("the handle MAC key slice has 32 bytes");
        let cleanup = Arc::new(HandleCleanupState::new());
        let objects = Arc::new(ObjectStore::new(session));
        Self {
            codec: TokenCodec::new(session, secret),
            phase: AtomicU8::new(HandleRegistryPhase::Open as u8),
            bindings: BindingTable::new(maximum_bindings),
            cleanup,
            objects,
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: Mutex::new(None),
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        self.objects.set_ghost(Arc::clone(&ghost));
        *self.ghost.lock() = Some(ghost);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        if let Some(ghost) = self.ghost.lock().as_ref().cloned() {
            ghost.record_event(event);
        }
    }

    #[cfg(all(target_os = "windows", any(test, feature = "shutdown-refinement")))]
    pub(crate) fn ghost_handle(&self) -> Option<crate::shutdown_refinement::GhostHandle> {
        self.ghost.lock().clone()
    }

    #[cfg(test)]
    #[must_use]
    pub fn new(maximum_bindings: usize) -> Self {
        Self::try_new(maximum_bindings).expect("test host provides an OS CSPRNG")
    }

    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        usize::try_from(self.bindings.read_state().live_bindings)
            .expect("binding count fits in usize")
    }

    pub(crate) fn phase(&self) -> HandleRegistryPhase {
        HandleRegistryPhase::from_raw(self.phase.load(Ordering::Acquire))
    }

    fn is_open(&self) -> bool {
        self.phase() == HandleRegistryPhase::Open
    }

    #[cfg(test)]
    pub(crate) fn insert_pending<T>(&self, value: &mut Option<T>) -> XllResult<String>
    where
        T: Send + Sync + 'static,
    {
        let object = ErasedObject::new(
            value.take().expect("pending handle value is armed"),
            Arc::clone(&self.cleanup),
        );
        let mut object = Some(object);
        self.insert_pending_object_with_kind::<T>(&mut object, None)
            .map(|(token, _binding_id, _object_id, _reused)| token)
    }

    pub(crate) fn insert_pending_object_with_kind<T>(
        &self,
        value: &mut Option<ErasedObject>,
        requested_object_id: Option<ObjectId>,
    ) -> XllResult<(String, HandleId, ObjectId, bool)>
    where
        T: Send + Sync + 'static,
    {
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let mut transaction = RegistryWriteTxn::reserve(self)?;

        let object = value.as_ref().expect("pending handle object is armed");
        if object.type_id != TypeId::of::<T>() {
            let actual_type = object.type_name;
            let _ = catch_unwind(AssertUnwindSafe(|| {
                tracing::warn!(
                    expected_type = type_name::<T>(),
                    actual_type,
                    "Excel handle object type mismatch"
                );
            }));
            return Err(XllError::InvalidHandle);
        }
        let object_id = match requested_object_id {
            Some(object_id) => object_id,
            None => self.objects.allocate_object_id()?,
        };
        let existing_key =
            requested_object_id.and_then(|_| transaction.objects().key_for_identity(object_id));
        let existing_object_ref = if let Some(existing_key) = existing_key {
            let entry = transaction
                .objects()
                .get(existing_key)
                .expect("object identity index must point at a live entry");
            if entry.value.type_id != TypeId::of::<T>() {
                let actual_type = entry.value.type_name;
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    tracing::warn!(
                        expected_type = type_name::<T>(),
                        actual_type,
                        "Excel handle alias type mismatch"
                    );
                }));
                return Err(XllError::InvalidHandle);
            }
            if entry.value.address() != object.address() {
                return Err(XllError::StaleHandle);
            }
            entry.bindings.checked_add(1).ok_or(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })?;
            Some(entry.value.published_ptr())
        } else {
            None
        };
        let (object_key, object_ref) = match existing_key {
            Some(existing_key) => {
                transaction.objects().add_binding(LiveObjectRef {
                    id: ObjectIdentity(object_id),
                    key: existing_key,
                })?;
                (
                    existing_key,
                    existing_object_ref.expect("existing object reference was validated above"),
                )
            }
            None => {
                let object_ref = value
                    .as_ref()
                    .expect("pending handle object is armed")
                    .published_ptr();
                let object_key = transaction.objects().insert(object_id, value)?;
                (object_key, object_ref)
            }
        };

        let (id, reused) = transaction.publish(
            LiveObjectRef {
                id: ObjectIdentity(object_id),
                key: object_key,
            },
            object_ref,
        );
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddHandle);
        Ok((self.codec.format(id), id, object_id, reused))
    }

    pub(crate) fn insert_existing_object_binding<T>(
        &self,
        object: ObjectLocator,
    ) -> XllResult<(String, HandleId, ObjectId, bool)>
    where
        T: ExcelHandleObject,
    {
        let object_id = object.id.0;
        let requested_object_key = object.key_hint;
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let mut transaction = RegistryWriteTxn::reserve(self)?;
        let live_key = transaction
            .objects()
            .get(requested_object_key)
            .map(|_| requested_object_key)
            .or_else(|| transaction.objects().key_for_identity(object_id));
        let (object_key, object_ref) = if let Some(object_key) = live_key {
            let entry = transaction
                .objects()
                .get(object_key)
                .expect("object identity index must point at a live entry");
            if entry.object_id != object_id {
                return Err(XllError::StaleHandle);
            }
            if entry.value.type_id != TypeId::of::<T>() {
                let actual_type = entry.value.type_name;
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    tracing::warn!(
                        expected_type = type_name::<T>(),
                        actual_type,
                        "Excel handle alias type mismatch"
                    );
                }));
                return Err(XllError::InvalidHandle);
            }
            entry.bindings.checked_add(1).ok_or(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })?;
            let object_ref = entry.value.published_ptr();
            transaction.objects().add_binding(LiveObjectRef {
                id: ObjectIdentity(object_id),
                key: object_key,
            })?;
            (object_key, object_ref)
        } else {
            let Some((object_key, object_ref)) = self.objects.resurrect(
                transaction.objects(),
                ObjectLocator {
                    id: ObjectIdentity(object_id),
                    key_hint: requested_object_key,
                },
                TypeId::of::<T>(),
                type_name::<T>(),
            )?
            else {
                return Err(XllError::StaleHandle);
            };
            (object_key, object_ref)
        };

        let (id, reused) = transaction.publish(
            LiveObjectRef {
                id: ObjectIdentity(object_id),
                key: object_key,
            },
            object_ref,
        );
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddHandle);
        Ok((self.codec.format(id), id, object_id, reused))
    }

    #[cfg(test)]
    pub fn lookup<T>(&self, token: &str) -> XllResult<T>
    where
        T: Send + Sync + Clone + 'static,
    {
        let verified = self
            .codec
            .parse(std::ptr::from_ref(self).addr(), HandleToken::new(token))?;
        let id = verified.id;
        let state = self.bindings.read_state();
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let slot = state
            .slots
            .get(id.slot as usize)
            .ok_or(XllError::StaleHandle)?;
        let record = slot
            .record
            .as_ref()
            .filter(|record| record.id == id)
            .ok_or(XllError::StaleHandle)?;
        let objects = self.objects.live.lock();
        let object = objects
            .get(record.object.key)
            .ok_or(XllError::StaleHandle)?;
        let object_ref = object.value.published_ptr();
        let Some(value) = object_ref.typed_ptr::<T>() else {
            let actual_type = object.value.type_name;
            drop(state);
            let _ = catch_unwind(AssertUnwindSafe(|| {
                tracing::warn!(
                    expected_type = type_name::<T>(),
                    actual_type,
                    "Excel handle type mismatch"
                );
            }));
            return Err(XllError::InvalidHandle);
        };
        // SAFETY: `value` points to the live data payload owned by the object
        // registry while the read lock is held.
        let value = unsafe { value.as_ref().clone() };
        drop(objects);
        drop(state);
        Ok(value)
    }

    pub(crate) fn lookup_handle<'call, T>(
        &self,
        scope: &'call crate::value::CallScope<'call>,
        token: &str,
    ) -> XllResult<Handle<'call, T>>
    where
        T: ExcelHandleObject,
    {
        let verified = self
            .codec
            .parse(std::ptr::from_ref(self).addr(), HandleToken::new(token))?;
        let id = verified.id;
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let domain = scope.handle_guard().register(&self.objects)?;
        let published_snapshot = self.bindings.published().load(id.slot);
        if let Some(record) = published_snapshot
            .get(id.slot)
            .filter(|record| record.id == id)
        {
            if !self.is_open() {
                return Err(XllError::Closing);
            }
            if record.state() != BindingState::Live {
                return Err(XllError::StaleHandle);
            }
            let Some(value) = record.object_ref.resolve::<T>(domain) else {
                let actual_type = record.object_ref.type_name;
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    tracing::warn!(
                        expected_type = type_name::<T>(),
                        actual_type,
                        "Excel handle type mismatch"
                    );
                }));
                return Err(XllError::InvalidHandle);
            };
            if record.state() != BindingState::Live {
                return Err(XllError::StaleHandle);
            }

            let object = record.object;
            drop(published_snapshot);
            return Ok(Handle::new(object, value));
        }
        drop(published_snapshot);

        self.lookup_handle_slow(id, domain)
    }

    fn lookup_handle_slow<'call, T>(
        &self,
        id: HandleId,
        domain: ObjectReadGuard<'call>,
    ) -> XllResult<Handle<'call, T>>
    where
        T: ExcelHandleObject,
    {
        let state = self.bindings.read_state();
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let slot = state
            .slots
            .get(id.slot as usize)
            .ok_or(XllError::StaleHandle)?;
        let record = slot
            .record
            .as_ref()
            .filter(|record| record.id == id)
            .ok_or(XllError::StaleHandle)?;
        let published_snapshot = self.bindings.published().load(id.slot);
        let record = published_snapshot
            .get(id.slot)
            .filter(|published| triomphe::Arc::ptr_eq(published, record))
            .ok_or(XllError::StaleHandle)?;
        if record.state() != BindingState::Live {
            return Err(XllError::StaleHandle);
        }
        let Some(value) = record.object_ref.resolve::<T>(domain) else {
            let actual_type = record.object_ref.type_name;
            drop(state);
            let _ = catch_unwind(AssertUnwindSafe(|| {
                tracing::warn!(
                    expected_type = type_name::<T>(),
                    actual_type,
                    "Excel handle type mismatch"
                );
            }));
            return Err(XllError::InvalidHandle);
        };

        let object = record.object;
        drop(state);
        Ok(Handle::new(object, value))
    }

    #[cfg(test)]
    pub(crate) fn remove<T>(&self, token: &str) -> XllResult<()>
    where
        T: Send + Sync + 'static,
    {
        let verified = self
            .codec
            .parse(std::ptr::from_ref(self).addr(), HandleToken::new(token))?;
        let id = verified.id;
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let mut transaction = RegistryRemovalTxn::begin(self, id)?;
        let object_key = transaction.object().key;
        let object = transaction
            .objects()
            .get(object_key)
            .ok_or(XllError::StaleHandle)?;
        if object.value.published_ptr().typed_ptr::<T>().is_none() {
            return Err(XllError::InvalidHandle);
        }
        let value = transaction.objects().release_binding(object_key);
        let _reusable = transaction.commit();
        if let Some(value) = value {
            self.objects.retire(value, "handle registry test removal");
        }
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        Ok(())
    }

    fn remove_with_kind(
        &self,
        token: &str,
        operation: &'static str,
        on_linearized: impl FnOnce(bool),
    ) -> XllResult<bool> {
        let verified = self
            .codec
            .parse(std::ptr::from_ref(self).addr(), HandleToken::new(token))?;
        let id = verified.id;
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let mut transaction = RegistryRemovalTxn::begin(self, id)?;
        let object_key = transaction.object().key;
        let value = transaction.objects().release_binding(object_key);
        let reusable = transaction.commit();
        on_linearized(reusable);
        if let Some(value) = value {
            self.objects.retire(value, operation);
        }
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        Ok(reusable)
    }

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        self.cleanup.result()
    }

    #[cfg(test)]
    pub(crate) fn remove_and_drop(&self, token: &str, operation: &'static str) {
        let _ = self.remove_and_drop_with_observer(token, operation, |_| {});
    }

    pub(crate) fn remove_and_drop_with_observer(
        &self,
        token: &str,
        operation: &'static str,
        on_linearized: impl FnOnce(bool),
    ) -> Option<bool> {
        self.remove_with_kind(token, operation, on_linearized).ok()
    }

    pub(crate) fn retire_values_for_seal(&self) -> usize {
        let live_bindings = self.bindings.retire_all();
        self.objects.seal();
        let values = self.objects.live.lock().take_all();
        self.objects.retire_all(values, "handle registry close");
        #[cfg(any(test, feature = "shutdown-refinement"))]
        for _ in 0..live_bindings {
            self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        }
        usize::try_from(live_bindings).expect("binding count fits in usize")
    }

    /// Reject new token resolutions while the runtime drains topic and
    /// prepare work. Actual value retirement remains in [`Self::seal`].
    pub(crate) fn begin_close(&self) {
        let _ = self.phase.compare_exchange(
            HandleRegistryPhase::Open as u8,
            HandleRegistryPhase::Closing as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// Seal the registry after the handle runtime has drained calls.
    ///
    /// This is intentionally restricted to the `handle` module. Callers must
    /// go through `HandleRuntime::seal`, which establishes the prepare/topic
    /// drain ordering before reaching this boundary.
    pub(super) fn seal(&self) -> XllResult<HandleRegistrySealed> {
        let previous = self
            .phase
            .swap(HandleRegistryPhase::Closing as u8, Ordering::AcqRel);
        if HandleRegistryPhase::from_raw(previous) == HandleRegistryPhase::Closed {
            self.cleanup_result()?;
            return Ok(HandleRegistrySealed::new());
        }
        self.retire_values_for_seal();
        self.objects.reclaim();
        self.phase
            .store(HandleRegistryPhase::Closed as u8, Ordering::Release);
        self.cleanup_result()?;
        Ok(HandleRegistrySealed::new())
    }

    pub(super) fn finish_quiescence(&self, _sealed: &HandleRegistrySealed) -> XllResult<()> {
        self.objects.finish_quiescence()?;
        Ok(())
    }
}
