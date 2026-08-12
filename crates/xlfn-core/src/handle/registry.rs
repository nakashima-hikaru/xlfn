use super::*;

pub(crate) struct HandleEntry {
    pub(crate) type_id: TypeId,
    pub(crate) type_name: &'static str,
    pub(crate) value: Arc<dyn Any + Send + Sync>,
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
    pub(crate) cleanup_failure: Mutex<Option<XllError>>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
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
            cleanup_failure: Mutex::new(None),
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

    pub(crate) fn insert_pending<T>(&self, value: &mut Option<Arc<T>>) -> XllResult<String>
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

        let (index, slot) = match state.free.pop() {
            Some(index) => {
                let slot = u32::try_from(index).map_err(|_| XllError::Internal {
                    diagnostic_id: 0x4841_4e44_534c_4f54,
                })?;
                (index, slot)
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
                (index, slot)
            }
        };
        let generation = state.slots[index].generation.max(1);
        let value = value.take().expect("pending handle value is armed");
        state.slots[index].entry = Some(HandleEntry {
            type_id: TypeId::of::<T>(),
            type_name: type_name::<T>(),
            value,
        });
        state.live += 1;
        drop(state);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddHandle);
        Ok(self.format_token(slot, generation))
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
        // close path first takes the corresponding write lock and therefore
        // cannot observe zero leases after this value has escaped.
        let lease = leases.acquire();
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
        let entry = slot.entry.as_ref().ok_or(XllError::StaleHandle)?;
        if entry.type_id != TypeId::of::<T>() {
            return Err(XllError::InvalidHandle);
        }
        if !entry.value.as_ref().is::<T>() {
            return Err(XllError::InvalidHandle);
        }
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

    pub(crate) fn remove_any(&self, token: &str) -> XllResult<Arc<dyn Any + Send + Sync>> {
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
        slot.entry.as_ref().ok_or(XllError::StaleHandle)?;
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
        Ok(entry.value)
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
        if let Ok(value) = self.remove_any(token) {
            self.drop_values(std::iter::once(value), operation);
        }
    }

    pub(crate) fn take_values_for_close(&self) -> Vec<Arc<dyn Any + Send + Sync>> {
        let mut state = self.state.write();
        state.closed = true;
        let live = state.live;
        let mut values = Vec::with_capacity(live);
        state.free.clear();
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
        // The write lock in take_values_for_close linearizes closure against
        // lookup_handle, which acquires its lease under the read lock.
        // Drop registry-held values first so any nested Handle<U> instances inside
        // registry-owned objects release their RuntimeLease before we wait.
        let values = self.take_values_for_close();
        self.drop_values(values, "handle registry close");
        leases.wait_for_idle();
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
