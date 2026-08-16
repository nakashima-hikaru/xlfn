use super::*;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandleRecordState {
    Live = 0,
    Retired = 1,
}

impl HandleRecordState {
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

/// Strongly owned storage for one shared handle object identity.
///
/// Published snapshots retain `Arc<HandleObject>` values. A formula binding
/// points at this object through its `ObjectId`, so aliases can create another
/// binding without cloning the business value. Destruction is centralized here
/// so consumer handles do not need ownership or cleanup behavior.
pub(crate) struct HandleObject {
    object_id: AtomicU64,
    value: ManuallyDrop<Arc<dyn Any + Send + Sync>>,
    cleanup: Arc<HandleCleanupState>,
}

impl HandleObject {
    pub(crate) fn new<T>(value: Arc<T>, cleanup: Arc<HandleCleanupState>) -> Arc<Self>
    where
        T: Any + Send + Sync + 'static,
    {
        let value: Arc<dyn Any + Send + Sync> = value;
        Arc::new(Self {
            object_id: AtomicU64::new(0),
            value: ManuallyDrop::new(value),
            cleanup,
        })
    }

    fn ensure_object_id(&self, candidate: ObjectId) -> ObjectId {
        let result =
            self.object_id
                .compare_exchange(0, candidate.0, Ordering::AcqRel, Ordering::Acquire);
        ObjectId(result.map_or_else(|existing| existing, |_| candidate.0))
    }

    fn object_id(&self) -> Option<ObjectId> {
        let value = self.object_id.load(Ordering::Acquire);
        match value {
            0 => None,
            value => Some(ObjectId(value)),
        }
    }

    pub(crate) fn get<T>(&self) -> Option<&T>
    where
        T: Any + Send + Sync + 'static,
    {
        (*self.value).as_ref().downcast_ref::<T>()
    }

    #[cfg(test)]
    fn clone_typed<T>(&self) -> Option<Arc<T>>
    where
        T: Any + Send + Sync + 'static,
    {
        Arc::downcast(Arc::clone(&*self.value)).ok()
    }
}

impl Drop for HandleObject {
    fn drop(&mut self) {
        let result = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: `value` is initialized exactly once and is never
            // accessed again after this final drop.
            unsafe { ManuallyDrop::drop(&mut self.value) };
        }));
        if result.is_err() {
            let error = XllError::Panic;
            crate::diagnostics::report_no_unwind("handle object drop", &error);
            self.cleanup.record(error);
        }
    }
}

/// One canonical formula-binding record shared by the mutable registry and
/// the immutable read-side publication snapshot.
///
/// Immutable ArcSwap snapshots are the reclamation barrier. Removing a
/// publication only makes it unavailable to new readers; an old snapshot
/// keeps both the publication and its object alive until the reader releases
/// the guard.
pub(crate) struct HandleRecord {
    pub(crate) id: HandleId,
    pub(crate) object_id: ObjectId,
    pub(crate) type_id: TypeId,
    pub(crate) type_name: &'static str,
    pub(crate) object: Arc<HandleObject>,
    pub(crate) state: AtomicU8,
}

impl HandleRecord {
    fn new(
        id: HandleId,
        object_id: ObjectId,
        type_id: TypeId,
        type_name: &'static str,
        object: Arc<HandleObject>,
    ) -> Self {
        Self {
            id,
            object_id,
            type_id,
            type_name,
            object,
            state: AtomicU8::new(HandleRecordState::Live as u8),
        }
    }

    pub(crate) fn state(&self) -> HandleRecordState {
        HandleRecordState::from_raw(self.state.load(Ordering::Acquire))
    }
}

const HANDLE_RECORD_CHUNK_SIZE: usize = 64;

#[derive(Clone)]
struct HandleRecordChunk {
    entries: Box<[Option<Arc<HandleRecord>>]>,
}

impl HandleRecordChunk {
    fn empty() -> Self {
        Self {
            entries: (0..HANDLE_RECORD_CHUNK_SIZE)
                .map(|_| None)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }
}

pub(crate) struct HandleRecordSnapshot {
    guard: arc_swap::Guard<Arc<HandleRecordChunk>>,
}

impl HandleRecordSnapshot {
    pub(crate) fn get(&self, slot: u32) -> Option<&Arc<HandleRecord>> {
        self.guard.entries[slot as usize & (HANDLE_RECORD_CHUNK_SIZE - 1)].as_ref()
    }
}

/// Immutable slot-indexed publication snapshots for warm handle lookup.
///
/// Slots are allocated from a bounded registry, so a hash table adds hashing
/// and probing without adding lookup information. Copy-on-write is limited to
/// one 64-entry chunk on insert/remove; readers perform only chunk selection
/// and indexed access.
pub(crate) struct PublishedHandles {
    chunks: Box<[ArcSwap<HandleRecordChunk>]>,
    empty: ArcSwap<HandleRecordChunk>,
}

impl PublishedHandles {
    pub(crate) fn new(maximum_handles: usize) -> Self {
        let chunk_count = maximum_handles.div_ceil(HANDLE_RECORD_CHUNK_SIZE).max(1);
        Self {
            chunks: (0..chunk_count)
                .map(|_| ArcSwap::from_pointee(HandleRecordChunk::empty()))
                .collect(),
            empty: ArcSwap::from_pointee(HandleRecordChunk::empty()),
        }
    }

    fn chunk_index(slot: u32) -> usize {
        slot as usize / HANDLE_RECORD_CHUNK_SIZE
    }

    /// Load the chunk containing one publication.
    ///
    /// The guard must remain alive while the caller validates and uses the
    /// borrowed publication. This avoids cloning the publication's `Arc` on
    /// every warm lookup while still keeping the immutable snapshot alive.
    pub(crate) fn load(&self, slot: u32) -> HandleRecordSnapshot {
        let chunk = self
            .chunks
            .get(Self::chunk_index(slot))
            .unwrap_or(&self.empty);
        HandleRecordSnapshot {
            guard: chunk.load(),
        }
    }

    /// Update the snapshot while the canonical registry write lock is held.
    fn insert(&self, id: HandleId, record: Arc<HandleRecord>) {
        let slot = id.slot;
        let Some(chunk) = self.chunks.get(Self::chunk_index(slot)) else {
            debug_assert!(false, "handle slot exceeds the publication table");
            return;
        };
        let current = chunk.load_full();
        let mut next = current.as_ref().clone();
        next.entries[slot as usize & (HANDLE_RECORD_CHUNK_SIZE - 1)] = Some(record);
        chunk.store(Arc::new(next));
    }

    /// Remove only the publication that belongs to the canonical entry being
    /// removed. The identity check keeps a future slot reuse from removing a
    /// newer generation if this helper is ever called outside the current
    /// write-lock discipline.
    fn remove(&self, id: HandleId, expected: &Arc<HandleRecord>) {
        let slot = id.slot;
        let Some(chunk) = self.chunks.get(Self::chunk_index(slot)) else {
            return;
        };
        let current = chunk.load_full();
        if !current.entries[slot as usize & (HANDLE_RECORD_CHUNK_SIZE - 1)]
            .as_ref()
            .is_some_and(|record| Arc::ptr_eq(record, expected))
        {
            return;
        }
        let mut next = current.as_ref().clone();
        next.entries[slot as usize & (HANDLE_RECORD_CHUNK_SIZE - 1)] = None;
        chunk.store(Arc::new(next));
    }

    /// Clear all publication snapshots while the canonical registry is being
    /// closed.
    fn clear(&self) {
        for chunk in &self.chunks {
            chunk.store(Arc::new(HandleRecordChunk::empty()));
        }
    }

    /// Find a live record for an object identity. This is intentionally a
    /// cold-path operation used only when publishing an explicit alias.
    fn find_object(&self, object_id: ObjectId) -> Option<Arc<HandleRecord>> {
        for chunk in &self.chunks {
            let snapshot = chunk.load();
            if let Some(record) = snapshot.entries.iter().flatten().find(|record| {
                record.object_id == object_id && record.state() == HandleRecordState::Live
            }) {
                return Some(Arc::clone(record));
            }
        }
        None
    }
}

pub(crate) struct Slot {
    pub(crate) next_generation: u64,
    pub(crate) record: Option<Arc<HandleRecord>>,
}

pub(crate) struct RegistryState {
    pub(crate) slots: Vec<Slot>,
    pub(crate) free: Vec<usize>,
    pub(crate) live: usize,
    pub(crate) next_object_id: u64,
}

pub(crate) struct HandleRegistry {
    pub(crate) session: u64,
    pub(crate) secret: [u8; 32],
    pub(crate) maximum_handles: usize,
    pub(crate) phase: AtomicU8,
    pub(crate) state: RwLock<RegistryState>,
    pub(crate) published: PublishedHandles,
    pub(crate) cleanup: Arc<HandleCleanupState>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
}

pub(crate) struct PendingHandleValue<'a> {
    registry: &'a HandleRegistry,
    value: Option<Arc<HandleObject>>,
    operation: &'static str,
}

impl<'a> PendingHandleValue<'a> {
    pub(crate) fn new(
        registry: &'a HandleRegistry,
        value: Arc<HandleObject>,
        operation: &'static str,
    ) -> Self {
        Self {
            registry,
            value: Some(value),
            operation,
        }
    }

    pub(crate) fn slot(&mut self) -> &mut Option<Arc<HandleObject>> {
        &mut self.value
    }
}

impl Drop for PendingHandleValue<'_> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            self.registry
                .drop_values(std::iter::once(value), self.operation);
        }
    }
}

pub(crate) const HANDLE_ENTROPY_DIAGNOSTIC_ID: u64 = 0x4841_4e44_524e_4746;

impl HandleRegistry {
    pub fn try_new(maximum_handles: usize) -> XllResult<Self> {
        Self::try_new_with(maximum_handles, |entropy| getrandom::fill(entropy), true)
    }

    pub(crate) fn try_new_with<E>(
        maximum_handles: usize,
        fill: impl FnOnce(&mut [u8; 40]) -> Result<(), E>,
        report_failure: bool,
    ) -> XllResult<Self>
    where
        E: std::fmt::Debug,
    {
        let mut entropy = [0_u8; 40];
        if let Err(source) = fill(&mut entropy) {
            let error = XllError::Internal {
                diagnostic_id: HANDLE_ENTROPY_DIAGNOSTIC_ID,
            };
            if report_failure {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    tracing::error!(
                        error = ?source,
                        diagnostic_id = HANDLE_ENTROPY_DIAGNOSTIC_ID,
                        "OS CSPRNG failed while initializing Excel handle tokens"
                    );
                }));
                crate::diagnostics::report_no_unwind("handle_registry_init", &error);
            }
            return Err(error);
        }
        Ok(Self::from_entropy(maximum_handles, entropy))
    }

    pub(crate) fn from_entropy(maximum_handles: usize, entropy: [u8; 40]) -> Self {
        let session = u64::from_le_bytes(
            entropy[..8]
                .try_into()
                .expect("the session entropy slice has eight bytes"),
        );
        let secret = entropy[8..]
            .try_into()
            .expect("the handle MAC key slice has 32 bytes");
        Self {
            session,
            secret,
            maximum_handles,
            phase: AtomicU8::new(HandleRegistryPhase::Open as u8),
            state: RwLock::new(RegistryState {
                slots: Vec::new(),
                free: Vec::new(),
                live: 0,
                next_object_id: 1,
            }),
            published: PublishedHandles::new(maximum_handles),
            cleanup: Arc::new(HandleCleanupState::new()),
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
    pub fn new(maximum_handles: usize) -> Self {
        Self::try_new(maximum_handles).expect("test host provides an OS CSPRNG")
    }

    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.state.read().live
    }

    pub(crate) fn phase(&self) -> HandleRegistryPhase {
        HandleRegistryPhase::from_raw(self.phase.load(Ordering::Acquire))
    }

    fn is_open(&self) -> bool {
        self.phase() == HandleRegistryPhase::Open
    }

    #[cfg(test)]
    pub(crate) fn insert_pending<T>(&self, value: &mut Option<Arc<T>>) -> XllResult<String>
    where
        T: Any + Send + Sync + 'static,
    {
        let object = HandleObject::new(
            value.take().expect("pending handle value is armed"),
            Arc::clone(&self.cleanup),
        );
        let mut object = Some(object);
        self.insert_pending_object_with_kind::<T>(&mut object)
            .map(|(token, _binding_id, _object_id, _reused)| token)
    }

    pub(crate) fn insert_pending_object_with_kind<T>(
        &self,
        value: &mut Option<Arc<HandleObject>>,
    ) -> XllResult<(String, HandleId, ObjectId, bool)>
    where
        T: Any + Send + Sync + 'static,
    {
        let mut state = self.state.write();
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        if state.live >= self.maximum_handles {
            return Err(XllError::Domain {
                code: DomainErrorCode::Overflow,
            });
        }

        let (index, slot, reused) = match state.free.pop() {
            Some(index) => {
                let slot = u32::try_from(index).map_err(|_| XllError::Internal {
                    diagnostic_id: 0x4841_4e44_534c_4f54,
                })?;
                (index, slot, true)
            }
            None => {
                let index = state.slots.len();
                let slot = u32::try_from(index).map_err(|_| XllError::Domain {
                    code: DomainErrorCode::Overflow,
                })?;
                state.slots.push(Slot {
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
        let object_id = if let Some(object_id) =
            value.as_ref().and_then(|object| object.object_id())
        {
            object_id
        } else {
            let candidate_object_id = ObjectId(state.next_object_id);
            state.next_object_id = state
                .next_object_id
                .checked_add(1)
                .ok_or(XllError::Domain {
                    code: DomainErrorCode::Overflow,
                })?;
            value
                .as_ref()
                .expect("pending handle object is armed")
                .ensure_object_id(candidate_object_id)
        };
        let object = value.take().expect("pending handle object is armed");
        let record = Arc::new(HandleRecord::new(
            id,
            object_id,
            TypeId::of::<T>(),
            type_name::<T>(),
            object,
        ));
        state.slots[index].record = Some(Arc::clone(&record));
        self.published.insert(id, record);
        state.live += 1;
        drop(state);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddHandle);
        Ok((self.format_token(id), id, object_id, reused))
    }

    /// Returns the shared object for an explicit alias operation.
    ///
    /// This is a cold-path ownership operation. The alias capability carries
    /// only the object identity; it never retains an `Arc` or a snapshot of
    /// its own. The object index is consulted only on this cold path.
    pub(crate) fn clone_object_for_binding<T>(
        &self,
        object_id: ObjectId,
    ) -> XllResult<Arc<HandleObject>>
    where
        T: ExcelHandleObject,
    {
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let record = self
            .published
            .find_object(object_id)
            .ok_or(XllError::StaleHandle)?;
        if record.type_id != TypeId::of::<T>() {
            return Err(XllError::InvalidHandle);
        }
        if record.state() != HandleRecordState::Live {
            return Err(XllError::StaleHandle);
        }
        Ok(Arc::clone(&record.object))
    }

    #[cfg(test)]
    pub fn lookup<T>(&self, token: &str) -> XllResult<Arc<T>>
    where
        T: Any + Send + Sync + 'static,
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
        if record.type_id != TypeId::of::<T>() {
            let actual_type = record.type_name;
            drop(state);
            let _ = catch_unwind(AssertUnwindSafe(|| {
                tracing::warn!(
                    expected_type = type_name::<T>(),
                    actual_type,
                    "Excel handle type mismatch"
                );
            }));
            return Err(XllError::InvalidHandle);
        }
        let value = record
            .object
            .clone_typed::<T>()
            .ok_or(XllError::InvalidHandle)?;
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
        let published_snapshot = self.published.load(id.slot);
        if let Some(record) = published_snapshot
            .get(id.slot)
            .filter(|record| record.id == id)
        {
            if !self.is_open() {
                return Err(XllError::Closing);
            }
            if record.type_id != TypeId::of::<T>() {
                let actual_type = record.type_name;
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    tracing::warn!(
                        expected_type = type_name::<T>(),
                        actual_type,
                        "Excel handle type mismatch"
                    );
                }));
                return Err(XllError::InvalidHandle);
            }

            match record.state() {
                HandleRecordState::Live => {}
                HandleRecordState::Retired => return Err(XllError::StaleHandle),
            }

            let value = record.object.get::<T>().ok_or(XllError::InvalidHandle)?;
            let value = NonNull::from(value);
            let object_id = record.object_id;
            return Ok(Handle::new(published_snapshot, object_id, value, scope));
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
            .filter(|published| Arc::ptr_eq(published, record))
            .ok_or(XllError::StaleHandle)?;
        if record.type_id != TypeId::of::<T>() {
            let actual_type = record.type_name;
            drop(state);
            let _ = catch_unwind(AssertUnwindSafe(|| {
                tracing::warn!(
                    expected_type = type_name::<T>(),
                    actual_type,
                    "Excel handle type mismatch"
                );
            }));
            return Err(XllError::InvalidHandle);
        }

        if record.state() != HandleRecordState::Live {
            return Err(XllError::StaleHandle);
        }
        let value = record.object.get::<T>().ok_or(XllError::InvalidHandle)?;
        let value = NonNull::from(value);
        let object_id = record.object_id;
        drop(state);
        Ok(Handle::new(published_snapshot, object_id, value, scope))
    }

    #[cfg(test)]
    pub(crate) fn remove<T>(&self, token: &str) -> XllResult<Arc<T>>
    where
        T: Any + Send + Sync + 'static,
    {
        let verified = self.parse_token(HandleToken::new(token))?;
        let id = verified.id;
        let mut state = self.state.write();
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let slot = state
            .slots
            .get_mut(id.slot as usize)
            .ok_or(XllError::StaleHandle)?;
        let record = slot
            .record
            .as_ref()
            .filter(|record| record.id == id)
            .ok_or(XllError::StaleHandle)?;
        if record.type_id != TypeId::of::<T>() {
            return Err(XllError::InvalidHandle);
        }
        if record.object.get::<T>().is_none() {
            return Err(XllError::InvalidHandle);
        }
        let record = Arc::clone(record);
        record
            .state
            .store(HandleRecordState::Retired as u8, Ordering::Release);
        self.published.remove(id, &record);
        drop(slot.record.take().expect("record was checked above"));
        let reusable = if let Some(next) = slot.next_generation.checked_add(1) {
            slot.next_generation = next;
            true
        } else {
            false
        };
        state.live -= 1;
        if reusable {
            state.free.push(id.slot as usize);
        }
        drop(state);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        record
            .object
            .clone_typed::<T>()
            .ok_or(XllError::InvalidHandle)
    }

    #[allow(
        dead_code,
        reason = "The kind-reporting wrapper is used by lifecycle trace production"
    )]
    pub(crate) fn remove_any(&self, token: &str) -> XllResult<Arc<HandleObject>> {
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        let result = self
            .remove_any_with_kind(token, |_| {})
            .map(|(value, _reusable)| value);
        #[cfg(not(any(test, feature = "handle-refinement-trace")))]
        let result = self
            .remove_any_with_kind(token)
            .map(|(value, _reusable)| value);
        result
    }

    fn remove_any_with_kind(
        &self,
        token: &str,
        #[cfg(any(test, feature = "handle-refinement-trace"))] on_linearized: impl FnOnce(bool),
    ) -> XllResult<(Arc<HandleObject>, bool)> {
        let verified = self.parse_token(HandleToken::new(token))?;
        let id = verified.id;
        let mut state = self.state.write();
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let slot = state
            .slots
            .get_mut(id.slot as usize)
            .ok_or(XllError::StaleHandle)?;
        let record = slot
            .record
            .as_ref()
            .filter(|record| record.id == id)
            .ok_or(XllError::StaleHandle)?;
        let record = Arc::clone(record);
        record
            .state
            .store(HandleRecordState::Retired as u8, Ordering::Release);
        self.published.remove(id, &record);
        drop(slot.record.take().expect("record was checked above"));
        let reusable = if let Some(next) = slot.next_generation.checked_add(1) {
            slot.next_generation = next;
            true
        } else {
            false
        };
        state.live -= 1;
        if reusable {
            state.free.push(id.slot as usize);
        }
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        on_linearized(reusable);
        drop(state);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        Ok((Arc::clone(&record.object), reusable))
    }

    pub(crate) fn record_cleanup_result(&self, result: XllResult<()>) {
        if let Err(error) = result {
            self.cleanup.record(error);
        }
    }

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        self.cleanup.result()
    }

    pub(crate) fn drop_values(
        &self,
        values: impl IntoIterator<Item = Arc<HandleObject>>,
        operation: &'static str,
    ) {
        self.record_cleanup_result(drop_handle_objects(values, operation));
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
        let result = self.remove_any_with_kind(token, |_| {});
        #[cfg(not(any(test, feature = "handle-refinement-trace")))]
        let result = self.remove_any_with_kind(token);
        if let Ok((value, reusable)) = result {
            self.drop_values(std::iter::once(value), operation);
            Some(reusable)
        } else {
            None
        }
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn remove_and_drop_with_trace(
        &self,
        token: &str,
        operation: &'static str,
        on_linearized: impl FnOnce(bool),
    ) -> Option<bool> {
        if let Ok((value, reusable)) = self.remove_any_with_kind(token, on_linearized) {
            self.drop_values(std::iter::once(value), operation);
            Some(reusable)
        } else {
            None
        }
    }

    pub(crate) fn take_values_for_close(&self) -> Vec<Arc<HandleObject>> {
        let mut state = self.state.write();
        let live = state.live;
        let mut values = Vec::with_capacity(live);
        state.free.clear();
        self.published.clear();
        for index in 0..state.slots.len() {
            let slot = &mut state.slots[index];
            if let Some(record) = slot.record.take() {
                record
                    .state
                    .store(HandleRecordState::Retired as u8, Ordering::Release);
                values.push(Arc::clone(&record.object));
            }
            if let Some(next) = slot.next_generation.checked_add(1) {
                slot.next_generation = next;
                state.free.push(index);
            }
        }
        state.live = 0;
        drop(state);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        for _ in 0..live {
            self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        }
        values
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

    pub(crate) fn close(&self) -> XllResult<()> {
        let previous = self
            .phase
            .swap(HandleRegistryPhase::Closing as u8, Ordering::AcqRel);
        if HandleRegistryPhase::from_raw(previous) == HandleRegistryPhase::Closed {
            return self.cleanup_result();
        }
        let values = self.take_values_for_close();
        self.drop_values(values, "handle registry close");
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
