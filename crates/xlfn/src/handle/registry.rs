use super::*;
use arc_swap::ArcSwapAny;
use std::cell::RefCell;
use std::ptr::NonNull;

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
/// This is deliberately a type-erased `Box<T>`, not an `Arc<T>`. The registry
/// owns the value for as long as at least one binding refers to its
/// [`ObjectKey`]. Call-scoped handles only borrow the payload while the epoch
/// guard is active.
///
/// # Invariants
///
/// - `ptr` was produced by `Box::into_raw(Box<T>)` for exactly one concrete
///   `T: Send + Sync + 'static`.
/// - `type_id == TypeId::of::<T>()`.
/// - `drop_value` is the monomorphized operation for that same `T`.
/// - The owner is moved exactly once into the object registry or retired
///   queue.
pub(crate) struct ErasedObject {
    ptr: NonNull<()>,
    type_id: TypeId,
    type_name: &'static str,
    drop_value: unsafe fn(NonNull<()>),
    cleanup: Arc<HandleCleanupState>,
    drop_operation: &'static str,
}

// SAFETY: construction only accepts `T: Send + Sync + 'static` and all
// type-erasure metadata is immutable and installed atomically by
// `new::<T>`.
unsafe impl Send for ErasedObject {}

// SAFETY: same invariant as Send; dereference is only exposed after
// TypeId validation as a shared `&T`.
unsafe impl Sync for ErasedObject {}

unsafe fn drop_value<T: Send + Sync + 'static>(ptr: NonNull<()>) {
    // SAFETY: ErasedObject construction produced `ptr` from
    // `Box::into_raw(Box<T>)` and this owner is consumed exactly once.
    unsafe {
        drop(Box::<T>::from_raw(ptr.cast::<T>().as_ptr()));
    }
}

impl ErasedObject {
    pub(crate) fn new<T: Send + Sync + 'static>(
        value: T,
        cleanup: Arc<HandleCleanupState>,
    ) -> Self {
        let ptr = Box::into_raw(Box::new(value));
        Self {
            ptr: NonNull::new(ptr.cast()).expect("Box::into_raw returns a non-null pointer"),
            type_id: TypeId::of::<T>(),
            type_name: type_name::<T>(),
            drop_value: drop_value::<T>,
            cleanup,
            drop_operation: "handle object drop",
        }
    }

    #[inline]
    pub(crate) fn typed_ptr<T: Send + Sync + 'static>(&self) -> Option<NonNull<T>> {
        (self.type_id == TypeId::of::<T>()).then(|| self.ptr.cast::<T>())
    }

    #[inline]
    pub(crate) fn address(&self) -> usize {
        self.ptr.as_ptr().addr()
    }

    fn set_drop_operation(&mut self, operation: &'static str) {
        self.drop_operation = operation;
    }
}

impl Drop for ErasedObject {
    fn drop(&mut self) {
        let result = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: this ErasedObject owns the allocation for the concrete
            // type associated with `drop_value`.
            unsafe {
                (self.drop_value)(self.ptr);
            }
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
/// Immutable ArcSwap snapshots publish binding metadata only. Object
/// reclamation is governed by the call epoch, so an old snapshot does not own
/// or extend the lifetime of the object payload.
pub(crate) struct BindingRecord {
    pub(crate) id: HandleId,
    pub(crate) object_id: ObjectId,
    pub(crate) object_key: ObjectKey,
    pub(crate) state: AtomicU8,
}

impl BindingRecord {
    fn new(id: HandleId, object_id: ObjectId, object_key: ObjectKey) -> Self {
        Self {
            id,
            object_id,
            object_key,
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
    pub(crate) fn new(maximum_bindings: usize) -> Self {
        let chunk_count = maximum_bindings.div_ceil(BINDING_CHUNK_SIZE).max(1);
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
    pub(crate) next_generation: u64,
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
    pub(crate) generation: u64,
}

/// Canonical ownership and binding-count metadata for one shared handle
/// object. Type metadata remains in [`ErasedObject`], the sole owner of the
/// payload, so it cannot diverge from the drop function.
pub(crate) struct ObjectEntry {
    pub(crate) object_id: ObjectId,
    pub(crate) bindings: usize,
    pub(crate) value: ErasedObject,
}

struct ObjectSlot {
    generation: u64,
    entry: Option<ObjectEntry>,
}

/// The sole owner of live handle objects.
///
/// Binding records and immutable publication snapshots contain only an
/// [`ObjectKey`]. Binding metadata is protected by the canonical handle state
/// lock and object storage by the runtime-local arena lock; readers borrow a
/// typed pointer for the duration of a call epoch.
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

    fn insert(&mut self, object_id: ObjectId, value: ErasedObject) -> XllResult<ObjectKey> {
        let index = match self.free.pop() {
            Some(index) => index,
            None => {
                let index = self.slots.len();
                u32::try_from(index).map_err(|_| XllError::Domain {
                    code: DomainErrorCode::Overflow,
                })?;
                self.slots.push(ObjectSlot {
                    generation: 1,
                    entry: None,
                });
                index
            }
        };

        let slot = u32::try_from(index).map_err(|_| XllError::Internal {
            diagnostic_id: crate::DiagnosticId::HANDLE_SLOT,
        })?;
        let generation = self.slots[index].generation.max(1);
        let key = ObjectKey {
            namespace: self.namespace,
            slot,
            generation,
        };
        let entry = ObjectEntry {
            object_id,
            bindings: 1,
            value,
        };
        debug_assert!(self.slots[index].entry.is_none());
        self.slots[index].entry = Some(entry);
        self.by_identity.insert(object_id, key);
        Ok(key)
    }

    fn add_binding(&mut self, key: ObjectKey, object_id: ObjectId) -> XllResult<()> {
        let entry = self.get_mut(key).ok_or(XllError::StaleHandle)?;
        if entry.object_id != object_id {
            return Err(XllError::StaleHandle);
        }
        entry.bindings = entry.bindings.checked_add(1).ok_or(XllError::Domain {
            code: DomainErrorCode::Overflow,
        })?;
        Ok(())
    }

    fn release_binding(&mut self, key: ObjectKey) -> Option<ErasedObject> {
        let index = key.slot as usize;
        let slot = self.slots.get_mut(index)?;
        if slot.generation != key.generation {
            debug_assert!(false, "binding references a stale object key");
            return None;
        }
        let entry = slot.entry.as_mut()?;
        debug_assert!(entry.bindings > 0);
        entry.bindings -= 1;
        if entry.bindings != 0 {
            return None;
        }

        let entry = slot.entry.take().expect("object entry was checked above");
        self.by_identity.remove(&entry.object_id);
        if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            self.free.push(index);
        }
        Some(entry.value)
    }

    fn take_all(&mut self) -> Vec<ErasedObject> {
        self.by_identity.clear();
        self.free.clear();
        let mut values = Vec::new();
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if let Some(entry) = slot.entry.take() {
                values.push(entry.value);
            }
            if let Some(next) = slot.generation.checked_add(1) {
                slot.generation = next;
                self.free.push(index);
            }
        }
        values
    }
}

struct RetiredObject {
    epoch: u64,
    value: ErasedObject,
}

struct EpochState {
    current: u64,
    active: FxHashMap<ThreadId, (u64, usize)>,
}

struct EpochDomain {
    state: Mutex<EpochState>,
}

impl EpochDomain {
    fn new() -> Self {
        Self {
            state: Mutex::new(EpochState {
                current: 1,
                active: FxHashMap::default(),
            }),
        }
    }

    fn enter(self: &Arc<Self>) -> EpochGuard {
        let thread = std::thread::current().id();
        let mut state = self.state.lock();
        let epoch = state.current;
        let active = state.active.entry(thread).or_insert((epoch, 0));
        active.1 = active.1.checked_add(1).expect("handle call depth overflow");
        EpochGuard {
            domain: Arc::clone(self),
            thread,
        }
    }

    fn retire_epoch(&self) -> u64 {
        let mut state = self.state.lock();
        let epoch = state.current;
        state.current = state.current.saturating_add(1);
        epoch
    }

    fn oldest_active(&self) -> Option<u64> {
        self.state
            .lock()
            .active
            .values()
            .map(|(epoch, _)| *epoch)
            .min()
    }
}

struct EpochGuard {
    domain: Arc<EpochDomain>,
    thread: ThreadId,
}

impl Drop for EpochGuard {
    fn drop(&mut self) {
        let mut state = self.domain.state.lock();
        let Some((_, count)) = state.active.get_mut(&self.thread) else {
            debug_assert!(false, "handle epoch guard is unbalanced");
            return;
        };
        *count = count.checked_sub(1).expect("handle epoch guard underflow");
        if *count == 0 {
            state.active.remove(&self.thread);
        }
    }
}

/// Reclamation queue shared with call scopes. A runtime-specific registration
/// is created only when a scope resolves a handle, so scalar Excel calls do
/// not acquire an epoch or touch the queue.
pub(crate) struct HandleReclaimer {
    live: Mutex<ObjectRegistry>,
    retired: Mutex<Vec<RetiredObject>>,
    epoch: Arc<EpochDomain>,
}

impl HandleReclaimer {
    fn new(namespace: u64) -> Self {
        Self {
            live: Mutex::new(ObjectRegistry::new(namespace)),
            retired: Mutex::new(Vec::new()),
            epoch: Arc::new(EpochDomain::new()),
        }
    }

    fn retire(&self, mut value: ErasedObject, operation: &'static str) {
        value.set_drop_operation(operation);
        let epoch = self.epoch.retire_epoch();
        self.retired.lock().push(RetiredObject { epoch, value });
        self.reclaim();
    }

    fn retire_all(&self, values: impl IntoIterator<Item = ErasedObject>, operation: &'static str) {
        let epoch = self.epoch.retire_epoch();
        let mut retired = self.retired.lock();
        retired.extend(values.into_iter().map(|mut value| {
            value.set_drop_operation(operation);
            RetiredObject { epoch, value }
        }));
        drop(retired);
        self.reclaim();
    }

    fn reclaim(&self) {
        let safe_before = self.epoch.oldest_active().unwrap_or(u64::MAX);
        let mut ready = Vec::new();
        {
            let mut retired = self.retired.lock();
            let mut pending = Vec::with_capacity(retired.len());
            for entry in retired.drain(..) {
                if entry.epoch < safe_before {
                    ready.push(entry.value);
                } else {
                    pending.push(entry);
                }
            }
            *retired = pending;
        }
        drop(ready);
    }
}

struct CallRegistration {
    reclaimer: Arc<HandleReclaimer>,
    epoch: EpochGuard,
}

/// Epoch participation for one Excel callback scope.
pub(crate) struct HandleCallGuard {
    reclaimers: RefCell<Vec<CallRegistration>>,
}

impl HandleCallGuard {
    pub(crate) fn new() -> Self {
        Self {
            reclaimers: RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn register(&self, reclaimer: &Arc<HandleReclaimer>) {
        let mut reclaimers = self.reclaimers.borrow_mut();
        if !reclaimers
            .iter()
            .any(|registration| Arc::ptr_eq(&registration.reclaimer, reclaimer))
        {
            reclaimers.push(CallRegistration {
                reclaimer: Arc::clone(reclaimer),
                epoch: reclaimer.epoch.enter(),
            });
        }
    }
}

impl Drop for HandleCallGuard {
    fn drop(&mut self) {
        let reclaimers = std::mem::take(self.reclaimers.get_mut());
        for registration in reclaimers {
            let reclaimer = registration.reclaimer;
            drop(registration.epoch);
            reclaimer.reclaim();
        }
    }
}

pub(crate) struct RegistryState {
    pub(crate) slots: Vec<BindingSlot>,
    pub(crate) free: Vec<usize>,
    pub(crate) live_bindings: usize,
    pub(crate) next_object_id: u64,
}

pub(crate) struct HandleRegistry {
    pub(crate) session: u64,
    pub(crate) secret: [u8; 32],
    pub(crate) maximum_bindings: usize,
    pub(crate) phase: AtomicU8,
    pub(crate) state: RwLock<RegistryState>,
    pub(crate) published: PublishedBindings,
    pub(crate) cleanup: Arc<HandleCleanupState>,
    pub(crate) reclaimer: Arc<HandleReclaimer>,
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
            self.registry.reclaimer.reclaim();
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
        let mut entropy = [0_u8; 40];
        if let Err(source) = fill(&mut entropy) {
            let error = XllError::Internal {
                diagnostic_id: crate::DiagnosticId::HANDLE_ENTROPY,
            };
            if report_failure {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    tracing::error!(
                        error = ?source,
                        diagnostic_id = crate::DiagnosticId::HANDLE_ENTROPY.as_u64(),
                        "OS CSPRNG failed while initializing Excel handle tokens"
                    );
                }));
                crate::diagnostics::report_no_unwind("handle_registry_init", &error);
            }
            return Err(error);
        }
        Ok(Self::from_entropy(maximum_bindings, entropy))
    }

    pub(crate) fn from_entropy(maximum_bindings: usize, entropy: [u8; 40]) -> Self {
        let session = u64::from_le_bytes(
            entropy[..8]
                .try_into()
                .expect("the session entropy slice has eight bytes"),
        );
        let secret = entropy[8..]
            .try_into()
            .expect("the handle MAC key slice has 32 bytes");
        let cleanup = Arc::new(HandleCleanupState::new());
        let reclaimer = Arc::new(HandleReclaimer::new(session));
        Self {
            session,
            secret,
            maximum_bindings,
            phase: AtomicU8::new(HandleRegistryPhase::Open as u8),
            state: RwLock::new(RegistryState {
                slots: Vec::new(),
                free: Vec::new(),
                live_bindings: 0,
                next_object_id: 1,
            }),
            published: PublishedBindings::new(maximum_bindings),
            cleanup,
            reclaimer,
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: Mutex::new(None),
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
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
        self.state.read().live_bindings
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
        let mut state = self.state.write();
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        if state.live_bindings >= self.maximum_bindings {
            return Err(XllError::Domain {
                code: DomainErrorCode::Overflow,
            });
        }
        let mut objects = self.reclaimer.live.lock();

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
        let object_id = requested_object_id.unwrap_or(ObjectId(state.next_object_id));
        let new_object_id = requested_object_id.is_none();
        let existing_key = requested_object_id.and_then(|_| objects.key_for_identity(object_id));
        if let Some(existing_key) = existing_key {
            let entry = objects
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
        }
        if new_object_id {
            state
                .next_object_id
                .checked_add(1)
                .ok_or(XllError::Domain {
                    code: DomainErrorCode::Overflow,
                })?;
        }

        let object_key = match existing_key {
            Some(existing_key) => {
                objects.add_binding(existing_key, object_id)?;
                existing_key
            }
            None => objects.insert(
                object_id,
                value.take().expect("pending handle object is armed"),
            )?,
        };

        let (index, slot, reused) = match state.free.pop() {
            Some(index) => {
                let slot = u32::try_from(index).map_err(|_| XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::HANDLE_SLOT,
                })?;
                (index, slot, true)
            }
            None => {
                let index = state.slots.len();
                let slot = u32::try_from(index).map_err(|_| XllError::Domain {
                    code: DomainErrorCode::Overflow,
                })?;
                state.slots.push(BindingSlot {
                    next_generation: 1,
                    record: None,
                });
                (index, slot, false)
            }
        };
        let id = HandleId {
            slot,
            generation: state.slots[index].next_generation.max(1),
        };
        if new_object_id {
            state.next_object_id += 1;
        }
        let record = triomphe::Arc::new(BindingRecord::new(id, object_id, object_key));
        state.slots[index].record = Some(triomphe::Arc::clone(&record));
        self.published.insert(id, record);
        state.live_bindings += 1;
        drop(objects);
        drop(state);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddHandle);
        Ok((self.format_token(id), id, object_id, reused))
    }

    pub(crate) fn insert_existing_object_binding<T>(
        &self,
        object_id: ObjectId,
        object_key: ObjectKey,
    ) -> XllResult<(String, HandleId, ObjectId, bool)>
    where
        T: ExcelHandleObject,
    {
        let mut state = self.state.write();
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        if state.live_bindings >= self.maximum_bindings {
            return Err(XllError::Domain {
                code: DomainErrorCode::Overflow,
            });
        }
        let mut objects = self.reclaimer.live.lock();
        let entry = objects.get(object_key).ok_or(XllError::StaleHandle)?;
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
        objects.add_binding(object_key, object_id)?;

        let (index, slot, reused) = match state.free.pop() {
            Some(index) => {
                let slot = u32::try_from(index).map_err(|_| XllError::Internal {
                    diagnostic_id: crate::DiagnosticId::HANDLE_SLOT,
                })?;
                (index, slot, true)
            }
            None => {
                let index = state.slots.len();
                let slot = u32::try_from(index).map_err(|_| XllError::Domain {
                    code: DomainErrorCode::Overflow,
                })?;
                state.slots.push(BindingSlot {
                    next_generation: 1,
                    record: None,
                });
                (index, slot, false)
            }
        };
        let id = HandleId {
            slot,
            generation: state.slots[index].next_generation.max(1),
        };
        let record = triomphe::Arc::new(BindingRecord::new(id, object_id, object_key));
        state.slots[index].record = Some(triomphe::Arc::clone(&record));
        self.published.insert(id, record);
        state.live_bindings += 1;
        drop(objects);
        drop(state);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddHandle);
        Ok((self.format_token(id), id, object_id, reused))
    }

    #[cfg(test)]
    pub fn lookup<T>(&self, token: &str) -> XllResult<T>
    where
        T: Send + Sync + Clone + 'static,
    {
        let verified = self.parse_token(HandleToken::new(token))?;
        let id = verified.id;
        let state = self.state.read();
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
        let objects = self.reclaimer.live.lock();
        let object = objects
            .get(record.object_key)
            .ok_or(XllError::StaleHandle)?;
        let Some(value) = object.value.typed_ptr::<T>() else {
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
        scope: &'call crate::CallScope<'call>,
        token: &str,
    ) -> XllResult<Handle<'call, T>>
    where
        T: ExcelHandleObject,
    {
        let verified = self.parse_token(HandleToken::new(token))?;
        let id = verified.id;
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        scope.register_handle_reclaimer(&self.reclaimer);
        let published_snapshot = self.published.load(id.slot);
        if let Some(record) = published_snapshot
            .get(id.slot)
            .filter(|record| record.id == id)
        {
            if !self.is_open() {
                return Err(XllError::Closing);
            }
            let state = self.state.read();
            let objects = self.reclaimer.live.lock();
            let object = objects
                .get(record.object_key)
                .ok_or(XllError::StaleHandle)?;
            let Some(value) = object.value.typed_ptr::<T>() else {
                let actual_type = object.value.type_name;
                drop(objects);
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

            match record.state() {
                BindingState::Live => {}
                BindingState::Retired => return Err(XllError::StaleHandle),
            }

            let object_id = record.object_id;
            let object_key = record.object_key;
            drop(objects);
            drop(state);
            return Ok(Handle::new(object_id, object_key, value, scope));
        }
        drop(published_snapshot);

        self.lookup_handle_slow(id, scope)
    }

    fn lookup_handle_slow<'call, T>(
        &self,
        id: HandleId,
        scope: &'call crate::CallScope<'call>,
    ) -> XllResult<Handle<'call, T>>
    where
        T: ExcelHandleObject,
    {
        let state = self.state.read();
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
        let published_snapshot = self.published.load(id.slot);
        let record = published_snapshot
            .get(id.slot)
            .filter(|published| triomphe::Arc::ptr_eq(published, record))
            .ok_or(XllError::StaleHandle)?;
        let objects = self.reclaimer.live.lock();
        let object = objects
            .get(record.object_key)
            .ok_or(XllError::StaleHandle)?;
        let Some(value) = object.value.typed_ptr::<T>() else {
            let actual_type = object.value.type_name;
            drop(objects);
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

        if record.state() != BindingState::Live {
            return Err(XllError::StaleHandle);
        }
        let object_id = record.object_id;
        let object_key = record.object_key;
        drop(objects);
        drop(state);
        Ok(Handle::new(object_id, object_key, value, scope))
    }

    #[cfg(test)]
    pub(crate) fn remove<T>(&self, token: &str) -> XllResult<()>
    where
        T: Send + Sync + 'static,
    {
        let verified = self.parse_token(HandleToken::new(token))?;
        let id = verified.id;
        let mut state = self.state.write();
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
        let mut objects = self.reclaimer.live.lock();
        let object = objects
            .get(record.object_key)
            .ok_or(XllError::StaleHandle)?;
        if object.value.typed_ptr::<T>().is_none() {
            return Err(XllError::InvalidHandle);
        }
        let record = triomphe::Arc::clone(record);
        let object_key = record.object_key;
        record
            .state
            .store(BindingState::Retired as u8, Ordering::Release);
        self.published.remove(id, &record);
        let slot = state
            .slots
            .get_mut(id.slot as usize)
            .expect("binding slot was checked above");
        drop(slot.record.take().expect("record was checked above"));
        let reusable = if let Some(next) = slot.next_generation.checked_add(1) {
            slot.next_generation = next;
            true
        } else {
            false
        };
        let value = objects.release_binding(object_key);
        drop(objects);
        state.live_bindings -= 1;
        if reusable {
            state.free.push(id.slot as usize);
        }
        drop(state);
        if let Some(value) = value {
            self.reclaimer.retire(value, "handle registry test removal");
        }
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        Ok(())
    }

    fn remove_with_kind(
        &self,
        token: &str,
        operation: &'static str,
        #[cfg(any(test, feature = "handle-refinement-trace"))] on_linearized: impl FnOnce(bool),
    ) -> XllResult<bool> {
        let verified = self.parse_token(HandleToken::new(token))?;
        let id = verified.id;
        let mut state = self.state.write();
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
        let mut objects = self.reclaimer.live.lock();
        let record = triomphe::Arc::clone(record);
        let object_key = record.object_key;
        record
            .state
            .store(BindingState::Retired as u8, Ordering::Release);
        self.published.remove(id, &record);
        let slot = state
            .slots
            .get_mut(id.slot as usize)
            .expect("binding slot was checked above");
        let slot_record = slot.record.take().expect("record was checked above");
        drop(slot_record);
        let reusable = if let Some(next) = slot.next_generation.checked_add(1) {
            slot.next_generation = next;
            true
        } else {
            false
        };
        let value = objects.release_binding(object_key);
        drop(objects);
        state.live_bindings -= 1;
        if reusable {
            state.free.push(id.slot as usize);
        }
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        on_linearized(reusable);
        drop(state);
        if let Some(value) = value {
            self.reclaimer.retire(value, operation);
        }
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        Ok(reusable)
    }

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        self.cleanup.result()
    }

    pub(crate) fn remove_and_drop(&self, token: &str, operation: &'static str) {
        let _ = self.remove_and_drop_with_kind(token, operation);
    }

    pub(crate) fn remove_and_drop_with_kind(
        &self,
        token: &str,
        operation: &'static str,
    ) -> Option<bool> {
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        let result = self.remove_with_kind(token, operation, |_| {});
        #[cfg(not(any(test, feature = "handle-refinement-trace")))]
        let result = self.remove_with_kind(token, operation);
        result.ok()
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn remove_and_drop_with_trace(
        &self,
        token: &str,
        operation: &'static str,
        on_linearized: impl FnOnce(bool),
    ) -> Option<bool> {
        self.remove_with_kind(token, operation, on_linearized).ok()
    }

    pub(crate) fn retire_values_for_close(&self) -> usize {
        let mut state = self.state.write();
        let live_bindings = state.live_bindings;
        state.free.clear();
        self.published.clear();
        for index in 0..state.slots.len() {
            let slot = &mut state.slots[index];
            if let Some(record) = slot.record.take() {
                record
                    .state
                    .store(BindingState::Retired as u8, Ordering::Release);
                drop(record);
            }
            if let Some(next) = slot.next_generation.checked_add(1) {
                slot.next_generation = next;
                state.free.push(index);
            }
        }
        let values = self.reclaimer.live.lock().take_all();
        state.live_bindings = 0;
        drop(state);
        self.reclaimer.retire_all(values, "handle registry close");
        #[cfg(any(test, feature = "shutdown-refinement"))]
        for _ in 0..live_bindings {
            self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        }
        live_bindings
    }

    /// Reject new token resolutions while the runtime drains topic and
    /// prepare work. Actual value retirement remains in `close`.
    pub(crate) fn begin_close(&self) {
        let _ = self.phase.compare_exchange(
            HandleRegistryPhase::Open as u8,
            HandleRegistryPhase::Closing as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// Complete terminal cleanup after the handle runtime has drained calls.
    ///
    /// This is intentionally restricted to the `handle` module. Callers must
    /// go through `HandleRuntime::close`, which establishes the prepare/topic
    /// drain ordering before reaching this boundary.
    pub(super) fn close(&self) -> XllResult<()> {
        let previous = self
            .phase
            .swap(HandleRegistryPhase::Closing as u8, Ordering::AcqRel);
        if HandleRegistryPhase::from_raw(previous) == HandleRegistryPhase::Closed {
            return self.cleanup_result();
        }
        self.retire_values_for_close();
        self.reclaimer.reclaim();
        self.phase
            .store(HandleRegistryPhase::Closed as u8, Ordering::Release);
        self.cleanup_result()
    }

    pub(crate) fn format_token(&self, id: HandleId) -> String {
        let tag = encode_tag(&self.authentication_tag(id));
        format!(
            "xllh:3:{:016x}:{:08x}:{:016x}:{tag}",
            self.session, id.slot, id.generation
        )
    }

    pub(crate) fn parse_token(&self, token: HandleToken<'_>) -> XllResult<VerifiedHandleToken> {
        let registry_address = std::ptr::from_ref(self).addr();
        if let Some(id) = verified_token_cache_lookup(
            registry_address,
            self.session,
            &self.secret,
            token.as_str(),
        ) {
            return Ok(VerifiedHandleToken { id });
        }

        let parsed = self.parse_token_uncached(token)?;
        let verified = self.verify_token(parsed)?;
        verified_token_cache_store(
            registry_address,
            self.session,
            &self.secret,
            token.as_str(),
            verified.id,
        );
        Ok(verified)
    }

    fn parse_token_uncached(&self, token: HandleToken<'_>) -> XllResult<ParsedHandleToken> {
        let mut fields = token.as_str().splitn(7, ':');
        let prefix = fields.next().ok_or(XllError::InvalidHandle)?;
        let version = fields.next().ok_or(XllError::InvalidHandle)?;
        let session = fields.next().ok_or(XllError::InvalidHandle)?;
        let slot = fields.next().ok_or(XllError::InvalidHandle)?;
        let generation = fields.next().ok_or(XllError::InvalidHandle)?;
        let tag = fields.next().ok_or(XllError::InvalidHandle)?;
        if fields.next().is_some()
            || prefix != "xllh"
            || version != "3"
            || session.len() != 16
            || slot.len() != 8
            || generation.len() != 16
            || tag.len() != 32
        {
            return Err(XllError::InvalidHandle);
        }
        let session = u64::from_str_radix(session, 16).map_err(|_| XllError::InvalidHandle)?;
        let slot = u32::from_str_radix(slot, 16).map_err(|_| XllError::InvalidHandle)?;
        let generation =
            u64::from_str_radix(generation, 16).map_err(|_| XllError::InvalidHandle)?;
        let tag = decode_tag(tag).ok_or(XllError::InvalidHandle)?;
        Ok(ParsedHandleToken {
            session,
            id: HandleId { slot, generation },
            tag,
        })
    }

    fn verify_token(&self, parsed: ParsedHandleToken) -> XllResult<VerifiedHandleToken> {
        let expected = self.authentication_tag(parsed.id);
        if parsed.session != self.session
            || !constant_time_eq::constant_time_eq(&parsed.tag, &expected)
        {
            return Err(XllError::InvalidHandle);
        }
        Ok(VerifiedHandleToken { id: parsed.id })
    }

    pub(crate) fn authentication_tag(&self, id: HandleId) -> [u8; 16] {
        let mut mac = blake3::Hasher::new_keyed(&self.secret);
        mac.update(b"xlfn-handle-token-v1\0");
        mac.update(&self.session.to_le_bytes());
        mac.update(&id.slot.to_le_bytes());
        mac.update(&id.generation.to_le_bytes());
        mac.finalize().as_bytes()[..16]
            .try_into()
            .expect("the BLAKE3 output contains a 128-bit tag")
    }
}
