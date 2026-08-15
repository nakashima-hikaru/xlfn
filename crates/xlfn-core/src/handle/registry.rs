use super::*;

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

/// A read-optimized registry entry that does not own the registered object.
///
/// The canonical `RegistryState` remains the only strong owner used for
/// lifecycle management. Keeping only a `Weak` here lets readers avoid the
/// registry lock without allowing an ArcSwap snapshot to delay destruction
/// during remove or terminal close.
pub(crate) struct PublishedHandle {
    pub(crate) generation: u64,
    pub(crate) type_id: TypeId,
    pub(crate) type_name: &'static str,
    pub(crate) value: Weak<dyn Any + Send + Sync>,
    pub(crate) state: AtomicU8,
}

impl PublishedHandle {
    fn new(
        generation: u64,
        type_id: TypeId,
        type_name: &'static str,
        value: &Arc<dyn Any + Send + Sync>,
    ) -> Self {
        Self {
            generation,
            type_id,
            type_name,
            value: Arc::downgrade(value),
            state: AtomicU8::new(PublishedHandleState::Live as u8),
        }
    }

    pub(crate) fn state(&self) -> PublishedHandleState {
        PublishedHandleState::from_raw(self.state.load(Ordering::Acquire))
    }

    pub(crate) fn upgrade(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.value.upgrade()
    }
}

type PublishedHandleMap = FxHashMap<u32, Arc<PublishedHandle>>;

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

    /// Load one publication while retaining no snapshot lock beyond the Arc
    /// clone needed to validate and use the publication.
    pub(crate) fn lookup(&self, slot: u32) -> Option<Arc<PublishedHandle>> {
        let snapshot = self.shards[Self::shard_index(slot)].load();
        snapshot.get(&slot).cloned()
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
    pub(crate) type_id: TypeId,
    pub(crate) type_name: &'static str,
    pub(crate) value: Arc<dyn Any + Send + Sync>,
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
    pub(crate) cleanup_failure: Mutex<Option<XllError>>,
    #[cfg(test)]
    pub(crate) before_fast_lease_acquire_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(crate) before_fast_upgrade_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) snapshot_recorder: Mutex<Option<Arc<SnapshotTraceRecorder>>>,
}

pub(crate) struct PendingHandleValue<'a, T>
where
    T: Any + Send + Sync + 'static,
{
    registry: &'a HandleRegistry,
    value: Option<Arc<T>>,
    operation: &'static str,
}

impl<'a, T> PendingHandleValue<'a, T>
where
    T: Any + Send + Sync + 'static,
{
    pub(crate) fn new(
        registry: &'a HandleRegistry,
        value: Arc<T>,
        operation: &'static str,
    ) -> Self {
        Self {
            registry,
            value: Some(value),
            operation,
        }
    }

    pub(crate) fn slot(&mut self) -> &mut Option<Arc<T>> {
        &mut self.value
    }
}

impl<T> Drop for PendingHandleValue<'_, T>
where
    T: Any + Send + Sync + 'static,
{
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            let value: Arc<dyn Any + Send + Sync> = value;
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
            cleanup_failure: Mutex::new(None),
            #[cfg(test)]
            before_fast_lease_acquire_hook: Mutex::new(None),
            #[cfg(test)]
            before_fast_upgrade_hook: Mutex::new(None),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: Mutex::new(None),
            #[cfg(any(test, feature = "handle-refinement-trace"))]
            snapshot_recorder: Mutex::new(None),
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

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn set_snapshot_trace_recorder(&self, recorder: Arc<SnapshotTraceRecorder>) {
        *self.snapshot_recorder.lock() = Some(recorder);
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn snapshot_trace_recorder(&self) -> Option<Arc<SnapshotTraceRecorder>> {
        self.snapshot_recorder.lock().as_ref().cloned()
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn next_snapshot_reader_id(&self) -> u64 {
        if let Some(recorder) = self.snapshot_trace_recorder() {
            recorder.next_reader_id()
        } else {
            1
        }
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn record_snapshot_trace_event(&self, event: SnapshotEvent) {
        if let Some(recorder) = self.snapshot_trace_recorder() {
            recorder.record(event);
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    #[cfg_attr(
        not(target_os = "windows"),
        allow(
            dead_code,
            reason = "Ghost handle only used in Windows COM shutdown path"
        )
    )]
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

    #[allow(
        dead_code,
        reason = "The kind-reporting wrapper is used by lifecycle trace production"
    )]
    pub(crate) fn insert_pending<T>(&self, value: &mut Option<Arc<T>>) -> XllResult<String>
    where
        T: Any + Send + Sync + 'static,
    {
        self.insert_pending_with_kind(value)
            .map(|(token, _reused)| token)
    }

    pub(crate) fn insert_pending_with_kind<T>(
        &self,
        value: &mut Option<Arc<T>>,
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
        let value: Arc<dyn Any + Send + Sync> =
            value.take().expect("pending handle value is armed");
        let publication = Arc::new(PublishedHandle::new(
            generation,
            TypeId::of::<T>(),
            type_name::<T>(),
            &value,
        ));
        state.slots[index].entry = Some(HandleEntry {
            type_id: TypeId::of::<T>(),
            type_name: type_name::<T>(),
            value,
            publication: Arc::clone(&publication),
        });
        self.published.insert(slot, publication);
        state.live += 1;
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        if reused {
            self.record_snapshot_trace_event(SnapshotEvent::InsertReuse {
                slot: slot as u64,
                generation,
            });
        } else {
            self.record_snapshot_trace_event(SnapshotEvent::InsertFresh);
        }
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
        if entry.type_id != TypeId::of::<T>() {
            let actual_type = entry.type_name;
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
        let value = Arc::clone(&entry.value);
        drop(state);
        Arc::downcast::<T>(value).map_err(|_| XllError::InvalidHandle)
    }

    pub(crate) fn lookup_handle<T>(
        &self,
        token: &str,
        leases: &Arc<HandleLeaseState>,
    ) -> XllResult<Handle<T>>
    where
        T: ExcelHandleObject,
    {
        let parsed = self.parse_token(token)?;
        if let Some(publication) = self.published.lookup(parsed.slot) {
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

            #[cfg(any(test, feature = "handle-refinement-trace"))]
            let reader_id = self.next_snapshot_reader_id();
            #[cfg(any(test, feature = "handle-refinement-trace"))]
            let token_wire = SnapshotTokenWire {
                session: self.session,
                slot: parsed.slot as u64,
                generation: parsed.generation,
            };
            #[cfg(any(test, feature = "handle-refinement-trace"))]
            self.record_snapshot_trace_event(SnapshotEvent::BeginFastObservation {
                reader_id,
                token: token_wire,
            });

            #[cfg(test)]
            if let Some(hook) = self.before_fast_lease_acquire_hook.lock().take() {
                hook();
            }

            // The first state check only avoids entering the lease path for
            // obviously withdrawn entries. The second check is the actual
            // lookup/close linearization point.
            let Some(lease) = leases.acquire() else {
                #[cfg(any(test, feature = "handle-refinement-trace"))]
                self.record_snapshot_trace_event(SnapshotEvent::AbandonObservation { reader_id });
                return Err(XllError::Closing);
            };
            #[cfg(any(test, feature = "handle-refinement-trace"))]
            self.record_snapshot_trace_event(SnapshotEvent::AcquireTentativeLease { reader_id });

            match publication.state() {
                PublishedHandleState::Live => {
                    #[cfg(any(test, feature = "handle-refinement-trace"))]
                    self.record_snapshot_trace_event(SnapshotEvent::ValidateFastLookup {
                        reader_id,
                    });
                }
                PublishedHandleState::Stale => {
                    #[cfg(any(test, feature = "handle-refinement-trace"))]
                    self.record_snapshot_trace_event(SnapshotEvent::RejectTentativeFastLookup {
                        reader_id,
                    });
                    drop(lease);
                    return Err(XllError::StaleHandle);
                }
                PublishedHandleState::Closing => {
                    #[cfg(any(test, feature = "handle-refinement-trace"))]
                    self.record_snapshot_trace_event(SnapshotEvent::RejectTentativeFastLookup {
                        reader_id,
                    });
                    drop(lease);
                    return Err(XllError::Closing);
                }
            }

            #[cfg(test)]
            if let Some(hook) = self.before_fast_upgrade_hook.lock().take() {
                hook();
            }

            let Some(value) = publication.upgrade() else {
                #[cfg(any(test, feature = "handle-refinement-trace"))]
                self.record_snapshot_trace_event(SnapshotEvent::FallbackFastLookup { reader_id });
                drop(lease);
                return self.lookup_handle_slow(parsed, leases);
            };
            let value = Arc::downcast::<T>(value).map_err(|_| XllError::InvalidHandle)?;
            #[cfg(any(test, feature = "handle-refinement-trace"))]
            let mut lease = lease;
            #[cfg(any(test, feature = "handle-refinement-trace"))]
            if let Some(recorder) = self.snapshot_trace_recorder() {
                lease.lineage = Some(LeaseLineageTrace::new_fast(recorder, reader_id));
            }
            return Ok(Handle {
                value: Some(value),
                lease: Some(lease),
            });
        }

        self.lookup_handle_slow(parsed, leases)
    }

    fn lookup_handle_slow<T>(
        &self,
        parsed: ParsedToken,
        leases: &Arc<HandleLeaseState>,
    ) -> XllResult<Handle<T>>
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
        if entry.type_id != TypeId::of::<T>() {
            let actual_type = entry.type_name;
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

        // Acquire the lease while the registry read lock is still held. The
        // close path seals lease admission before taking the corresponding
        // write lock, so it cannot observe zero leases while a new lease can
        // still escape this read-side critical section.
        let Some(lease) = leases.acquire() else {
            return Err(XllError::Closing);
        };
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        let token_wire = SnapshotTokenWire {
            session: self.session,
            slot: parsed.slot as u64,
            generation: parsed.generation,
        };
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        self.record_snapshot_trace_event(SnapshotEvent::BeginSlowLookup { token: token_wire });
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        let mut lease = lease;
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        if let Some(recorder) = self.snapshot_trace_recorder() {
            lease.lineage = Some(LeaseLineageTrace::new_slow(recorder));
        }
        let value = Arc::clone(&entry.value);
        drop(state);
        let value = Arc::downcast::<T>(value).map_err(|_| XllError::InvalidHandle)?;
        Ok(Handle {
            value: Some(value),
            lease: Some(lease),
        })
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
            if entry.type_id != TypeId::of::<T>() {
                return Err(XllError::InvalidHandle);
            }
            if !entry.value.as_ref().is::<T>() {
                return Err(XllError::InvalidHandle);
            }
            Arc::clone(&entry.publication)
        };
        publication
            .state
            .store(PublishedHandleState::Stale as u8, Ordering::Release);
        self.published.remove(parsed.slot, &publication);
        let entry = slot.entry.take().expect("entry was checked above");
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
        Arc::downcast::<T>(entry.value).map_err(|_| XllError::InvalidHandle)
    }

    #[allow(
        dead_code,
        reason = "The kind-reporting wrapper is used by lifecycle trace production"
    )]
    pub(crate) fn remove_any(&self, token: &str) -> XllResult<Arc<dyn Any + Send + Sync>> {
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
    ) -> XllResult<(Arc<dyn Any + Send + Sync>, bool)> {
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
        let entry = slot.entry.take().expect("entry was checked above");
        let reusable = if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            true
        } else {
            false
        };
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        let updated_generation = slot.generation;
        state.live -= 1;
        if reusable {
            state.free.push(parsed.slot as usize);
        }
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        let token_wire = SnapshotTokenWire {
            session: self.session,
            slot: parsed.slot as u64,
            generation: parsed.generation,
        };
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        if reusable {
            self.record_snapshot_trace_event(SnapshotEvent::RemoveReuse {
                token: token_wire,
                next_generation: updated_generation,
            });
        } else {
            self.record_snapshot_trace_event(SnapshotEvent::RemoveRetire { token: token_wire });
        }
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        on_linearized(reusable);
        drop(state);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        Ok((entry.value, reusable))
    }

    pub(crate) fn record_cleanup_result(&self, result: XllResult<()>) {
        if let Err(error) = result {
            let mut failure = self.cleanup_failure.lock();
            if failure.is_none() {
                *failure = Some(error);
            }
        }
    }

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        let failure = self.cleanup_failure.lock();
        match failure.as_ref() {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    pub(crate) fn drop_values(
        &self,
        values: impl IntoIterator<Item = Arc<dyn Any + Send + Sync>>,
        operation: &'static str,
    ) {
        self.record_cleanup_result(drop_handle_values(values, operation));
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
    #[allow(
        dead_code,
        reason = "The trace callback is used by Windows/test lifecycle paths"
    )]
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

    pub(crate) fn take_values_for_close(&self) -> Vec<Arc<dyn Any + Send + Sync>> {
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
                values.push(entry.value);
            }
            if let Some(next) = slot.generation.checked_add(1) {
                slot.generation = next;
                state.free.push(index);
            }
        }
        state.live = 0;
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        self.record_snapshot_trace_event(SnapshotEvent::CloseRegistry);
        drop(state);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        for _ in 0..live {
            self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        }
        values
    }

    #[cfg(test)]
    pub fn close(&self) -> XllResult<()> {
        let values = self.take_values_for_close();
        self.drop_values(values, "handle registry close");
        self.cleanup_result()
    }

    pub(crate) fn close_with_leases(&self, leases: &HandleLeaseState) -> XllResult<()> {
        // Seal lease admission before removing canonical values. Existing
        // leases may still drain, but no new independent lookup lease can be
        // admitted after this point.
        leases.seal();
        // Drop registry-held values first so any nested Handle<U> instances inside
        // registry-owned objects release their RuntimeLease before we wait.
        let values = self.take_values_for_close();
        self.drop_values(values, "handle registry close");
        leases.wait_for_idle();
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        self.record_snapshot_trace_event(SnapshotEvent::FinishClose);
        let lease_cleanup = leases.cleanup_result();
        match lease_cleanup {
            Ok(()) => self.cleanup_result(),
            Err(error) => Err(error),
        }
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
