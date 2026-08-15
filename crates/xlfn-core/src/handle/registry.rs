use super::*;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;

const PUBLISHED_HANDLE_SHARD_COUNT: usize = 64;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublishedHandleState {
    Live = 0,
    Stale = 1,
    Closing = 2,
}

impl PublishedHandleState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            value if value == Self::Live as u8 => Self::Live,
            value if value == Self::Stale as u8 => Self::Stale,
            value if value == Self::Closing as u8 => Self::Closing,
            _ => Self::Stale,
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

/// Strongly owned storage for one formula-owned handle object.
///
/// Published snapshots retain `Arc<HandleObject>` values. The object therefore
/// remains alive for every borrowed `Handle` whose snapshot guard still points
/// at the publication. Destruction is centralized here so consumer handles do
/// not need ownership or cleanup behavior.
pub(crate) struct HandleObject {
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
            value: ManuallyDrop::new(value),
            cleanup,
        })
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

/// A read-optimized registry entry that strongly owns the registered object.
///
/// Immutable ArcSwap snapshots are the reclamation barrier. Removing a
/// publication only makes it unavailable to new readers; an old snapshot
/// keeps both the publication and its object alive until the reader releases
/// the guard.
pub(crate) struct PublishedHandle {
    pub(crate) generation: u64,
    pub(crate) type_id: TypeId,
    pub(crate) type_name: &'static str,
    pub(crate) object: Arc<HandleObject>,
    pub(crate) state: AtomicU8,
}

impl PublishedHandle {
    fn new(
        generation: u64,
        type_id: TypeId,
        type_name: &'static str,
        object: Arc<HandleObject>,
    ) -> Self {
        Self {
            generation,
            type_id,
            type_name,
            object,
            state: AtomicU8::new(PublishedHandleState::Live as u8),
        }
    }

    pub(crate) fn state(&self) -> PublishedHandleState {
        PublishedHandleState::from_raw(self.state.load(Ordering::Acquire))
    }
}

pub(crate) type PublishedHandleMap = FxHashMap<u32, Arc<PublishedHandle>>;
pub(crate) type PublishedHandleSnapshot = arc_swap::Guard<Arc<PublishedHandleMap>>;

/// Sharded immutable publication snapshots for warm handle lookup.
pub(crate) struct PublishedHandles {
    shards: [ArcSwap<PublishedHandleMap>; PUBLISHED_HANDLE_SHARD_COUNT],
}

impl PublishedHandles {
    pub(crate) fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| ArcSwap::from_pointee(PublishedHandleMap::default())),
        }
    }

    fn shard_index(slot: u32) -> usize {
        (slot as usize) & (PUBLISHED_HANDLE_SHARD_COUNT - 1)
    }

    /// Load the shard containing one publication.
    ///
    /// The guard must remain alive while the caller validates and uses the
    /// borrowed publication. This avoids cloning the publication's `Arc` on
    /// every warm lookup while still keeping the immutable snapshot alive.
    pub(crate) fn load(&self, slot: u32) -> PublishedHandleSnapshot {
        self.shards[Self::shard_index(slot)].load()
    }

    /// Update the snapshot while the canonical registry write lock is held.
    fn insert(&self, slot: u32, publication: Arc<PublishedHandle>) {
        let shard = &self.shards[Self::shard_index(slot)];
        let current = shard.load_full();
        let mut next = current.as_ref().clone();
        next.insert(slot, publication);
        shard.store(Arc::new(next));
    }

    /// Remove only the publication that belongs to the canonical entry being
    /// removed. The identity check keeps a future slot reuse from removing a
    /// newer generation if this helper is ever called outside the current
    /// write-lock discipline.
    fn remove(&self, slot: u32, expected: &Arc<PublishedHandle>) {
        let shard = &self.shards[Self::shard_index(slot)];
        let current = shard.load_full();
        if !current
            .get(&slot)
            .is_some_and(|publication| Arc::ptr_eq(publication, expected))
        {
            return;
        }
        let mut next = current.as_ref().clone();
        next.remove(&slot);
        shard.store(Arc::new(next));
    }

    /// Clear all publication snapshots while the canonical registry is being
    /// closed.
    fn clear(&self) {
        for shard in &self.shards {
            shard.store(Arc::new(PublishedHandleMap::default()));
        }
    }
}

pub(crate) struct HandleEntry {
    pub(crate) publication: Arc<PublishedHandle>,
}

pub(crate) struct Slot {
    pub(crate) generation: u64,
    pub(crate) entry: Option<HandleEntry>,
}

pub(crate) struct RegistryState {
    pub(crate) slots: Vec<Slot>,
    pub(crate) free: Vec<usize>,
    pub(crate) live: usize,
    pub(crate) closed: bool,
}

pub(crate) struct HandleRegistry {
    pub(crate) session: u64,
    pub(crate) secret: [u8; 32],
    pub(crate) maximum_handles: usize,
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
            state: RwLock::new(RegistryState {
                slots: Vec::new(),
                free: Vec::new(),
                live: 0,
                closed: false,
            }),
            published: PublishedHandles::new(),
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
            .map(|(token, _reused)| token)
    }

    pub(crate) fn insert_pending_object_with_kind<T>(
        &self,
        value: &mut Option<Arc<HandleObject>>,
    ) -> XllResult<(String, bool)>
    where
        T: Any + Send + Sync + 'static,
    {
        let mut state = self.state.write();
        if state.closed {
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
                    generation: 1,
                    entry: None,
                });
                (index, slot, false)
            }
        };
        let generation = state.slots[index].generation.max(1);
        let publication = Arc::new(PublishedHandle::new(
            generation,
            TypeId::of::<T>(),
            type_name::<T>(),
            value.take().expect("pending handle object is armed"),
        ));
        state.slots[index].entry = Some(HandleEntry {
            publication: Arc::clone(&publication),
        });
        self.published.insert(slot, publication);
        state.live += 1;
        drop(state);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddHandle);
        Ok((self.format_token(slot, generation), reused))
    }

    #[cfg(test)]
    pub fn lookup<T>(&self, token: &str) -> XllResult<Arc<T>>
    where
        T: Any + Send + Sync + 'static,
    {
        let parsed = self.parse_token(token)?;
        let state = self.state.read();
        if state.closed {
            return Err(XllError::Closing);
        }
        let slot = state
            .slots
            .get(parsed.slot as usize)
            .ok_or(XllError::StaleHandle)?;
        if slot.generation != parsed.generation {
            return Err(XllError::StaleHandle);
        }
        let entry = slot.entry.as_ref().ok_or(XllError::StaleHandle)?;
        if entry.publication.type_id != TypeId::of::<T>() {
            let actual_type = entry.publication.type_name;
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
        let value = entry
            .publication
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
        let parsed = self.parse_token(token)?;
        let published_snapshot = self.published.load(parsed.slot);
        if let Some(publication) = published_snapshot.get(&parsed.slot) {
            if publication.generation != parsed.generation {
                return Err(XllError::StaleHandle);
            }
            if publication.type_id != TypeId::of::<T>() {
                let actual_type = publication.type_name;
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    tracing::warn!(
                        expected_type = type_name::<T>(),
                        actual_type,
                        "Excel handle type mismatch"
                    );
                }));
                return Err(XllError::InvalidHandle);
            }

            match publication.state() {
                PublishedHandleState::Live => {}
                PublishedHandleState::Stale => return Err(XllError::StaleHandle),
                PublishedHandleState::Closing => return Err(XllError::Closing),
            }

            let value = publication
                .object
                .get::<T>()
                .ok_or(XllError::InvalidHandle)?;
            let value = NonNull::from(value);
            return Ok(Handle::new(published_snapshot, parsed.slot, value, scope));
        }
        drop(published_snapshot);

        self.lookup_handle_slow(parsed, scope)
    }

    fn lookup_handle_slow<'call, T>(
        &self,
        parsed: ParsedToken,
        scope: &'call crate::CallScope<'call>,
    ) -> XllResult<Handle<'call, T>>
    where
        T: ExcelHandleObject,
    {
        let state = self.state.read();
        if state.closed {
            return Err(XllError::Closing);
        }
        let slot = state
            .slots
            .get(parsed.slot as usize)
            .ok_or(XllError::StaleHandle)?;
        if slot.generation != parsed.generation {
            return Err(XllError::StaleHandle);
        }
        let entry = slot.entry.as_ref().ok_or(XllError::StaleHandle)?;
        let published_snapshot = self.published.load(parsed.slot);
        let publication = published_snapshot
            .get(&parsed.slot)
            .filter(|publication| Arc::ptr_eq(publication, &entry.publication))
            .ok_or(XllError::StaleHandle)?;
        if publication.type_id != TypeId::of::<T>() {
            let actual_type = publication.type_name;
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

        if publication.state() != PublishedHandleState::Live {
            return match publication.state() {
                PublishedHandleState::Stale => Err(XllError::StaleHandle),
                PublishedHandleState::Closing => Err(XllError::Closing),
                PublishedHandleState::Live => unreachable!(),
            };
        }
        let value = publication
            .object
            .get::<T>()
            .ok_or(XllError::InvalidHandle)?;
        let value = NonNull::from(value);
        drop(state);
        Ok(Handle::new(published_snapshot, parsed.slot, value, scope))
    }

    #[cfg(test)]
    pub(crate) fn remove<T>(&self, token: &str) -> XllResult<Arc<T>>
    where
        T: Any + Send + Sync + 'static,
    {
        let parsed = self.parse_token(token)?;
        let mut state = self.state.write();
        if state.closed {
            return Err(XllError::Closing);
        }
        let slot = state
            .slots
            .get_mut(parsed.slot as usize)
            .ok_or(XllError::StaleHandle)?;
        if slot.generation != parsed.generation {
            return Err(XllError::StaleHandle);
        }
        let publication = {
            let entry = slot.entry.as_ref().ok_or(XllError::StaleHandle)?;
            if entry.publication.type_id != TypeId::of::<T>() {
                return Err(XllError::InvalidHandle);
            }
            if entry.publication.object.get::<T>().is_none() {
                return Err(XllError::InvalidHandle);
            }
            Arc::clone(&entry.publication)
        };
        publication
            .state
            .store(PublishedHandleState::Stale as u8, Ordering::Release);
        self.published.remove(parsed.slot, &publication);
        drop(slot.entry.take().expect("entry was checked above"));
        let reusable = if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            true
        } else {
            false
        };
        state.live -= 1;
        if reusable {
            state.free.push(parsed.slot as usize);
        }
        drop(state);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        publication
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
        let parsed = self.parse_token(token)?;
        let mut state = self.state.write();
        if state.closed {
            return Err(XllError::Closing);
        }
        let slot = state
            .slots
            .get_mut(parsed.slot as usize)
            .ok_or(XllError::StaleHandle)?;
        if slot.generation != parsed.generation {
            return Err(XllError::StaleHandle);
        }
        let publication = {
            let entry = slot.entry.as_ref().ok_or(XllError::StaleHandle)?;
            Arc::clone(&entry.publication)
        };
        publication
            .state
            .store(PublishedHandleState::Stale as u8, Ordering::Release);
        self.published.remove(parsed.slot, &publication);
        drop(slot.entry.take().expect("entry was checked above"));
        let reusable = if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            true
        } else {
            false
        };
        state.live -= 1;
        if reusable {
            state.free.push(parsed.slot as usize);
        }
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        on_linearized(reusable);
        drop(state);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        Ok((Arc::clone(&publication.object), reusable))
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
        state.closed = true;
        let live = state.live;
        let mut values = Vec::with_capacity(live);
        state.free.clear();
        for slot in &state.slots {
            if let Some(entry) = slot.entry.as_ref() {
                entry
                    .publication
                    .state
                    .store(PublishedHandleState::Closing as u8, Ordering::Release);
            }
        }
        self.published.clear();
        for index in 0..state.slots.len() {
            let slot = &mut state.slots[index];
            if let Some(entry) = slot.entry.take() {
                values.push(Arc::clone(&entry.publication.object));
            }
            if let Some(next) = slot.generation.checked_add(1) {
                slot.generation = next;
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

    pub(crate) fn close(&self) -> XllResult<()> {
        let values = self.take_values_for_close();
        self.drop_values(values, "handle registry close");
        self.cleanup_result()
    }

    pub(crate) fn format_token(&self, slot: u32, generation: u64) -> String {
        let tag = encode_tag(&self.authentication_tag(slot, generation));
        format!(
            "xllh:3:{:016x}:{slot:08x}:{generation:016x}:{tag}",
            self.session
        )
    }

    pub(crate) fn parse_token(&self, token: &str) -> XllResult<ParsedToken> {
        let registry_address = std::ptr::from_ref(self).addr();
        if let Some(parsed) =
            verified_token_cache_lookup(registry_address, self.session, &self.secret, token)
        {
            return Ok(parsed);
        }

        let parsed = self.parse_token_uncached(token)?;
        verified_token_cache_store(registry_address, self.session, &self.secret, token, parsed);
        Ok(parsed)
    }

    fn parse_token_uncached(&self, token: &str) -> XllResult<ParsedToken> {
        let mut fields = token.splitn(7, ':');
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
        let expected = self.authentication_tag(slot, generation);
        if session != self.session || !constant_time_eq::constant_time_eq(&tag, &expected) {
            return Err(XllError::InvalidHandle);
        }
        Ok(ParsedToken { slot, generation })
    }

    pub(crate) fn authentication_tag(&self, slot: u32, generation: u64) -> [u8; 16] {
        let mut mac = blake3::Hasher::new_keyed(&self.secret);
        mac.update(b"xlfn-handle-token-v1\0");
        mac.update(&self.session.to_le_bytes());
        mac.update(&slot.to_le_bytes());
        mac.update(&generation.to_le_bytes());
        mac.finalize().as_bytes()[..16]
            .try_into()
            .expect("the BLAKE3 output contains a 128-bit tag")
    }
}
