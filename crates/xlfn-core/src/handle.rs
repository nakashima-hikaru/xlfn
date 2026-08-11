use crate::{DomainErrorCode, ExcelCallbackStatus, ReturnContext, XllError, XllResult};
use parking_lot::{Condvar, Mutex, RwLock};
use std::any::{Any, TypeId, type_name};
use std::cell::Cell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::ops::Deref;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
#[cfg(any(target_os = "windows", test))]
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::ThreadId;

/// Marker implemented by `#[derive(ExcelHandleObject)]`.
///
/// A handle-producing UDF is memoized by its formula identity.
/// For one live formula identity, the producer is evaluated at most once
/// and the resulting handle token identifies that object for the token's
/// entire lifetime.
///
/// Producers must therefore depend only on their Excel-visible inputs and
/// stable application state explicitly represented by those inputs.
pub trait ExcelHandleObject: Any + Send + Sync + 'static {}

/// A typed, call-safe reference to an object owned by an Excel handle topic.
pub struct Handle<T: ExcelHandleObject> {
    // Options let Drop release the value under a panic boundary before
    // returning the runtime lease. This records a destructor failure even when
    // a formula topic was already removed and the Handle owns the final Arc.
    value: Option<Arc<T>>,
    lease: Option<HandleLease>,
}

impl<T: ExcelHandleObject> Handle<T> {
    pub(crate) fn into_arc(mut self) -> Arc<T> {
        let value = self
            .value
            .take()
            .expect("a live Handle contains its object reference");
        // The caller immediately republishes this Arc while its UDF CallGuard
        // still prevents terminal handle shutdown.
        drop(self.lease.take());
        value
    }
}

impl<T: ExcelHandleObject> Clone for Handle<T> {
    fn clone(&self) -> Self {
        Self {
            value: Some(Arc::clone(
                self.value
                    .as_ref()
                    .expect("a live Handle contains its object reference"),
            )),
            lease: Some(
                self.lease
                    .as_ref()
                    .expect("a live Handle contains its runtime lease")
                    .clone(),
            ),
        }
    }
}

impl<T: ExcelHandleObject> Deref for Handle<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value
            .as_deref()
            .expect("a live Handle contains its object reference")
    }
}

impl<T: ExcelHandleObject> Drop for Handle<T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take()
            && catch_unwind(AssertUnwindSafe(|| drop(value))).is_err()
        {
            let error = XllError::Panic;
            crate::diagnostics::report_no_unwind("handle lease drop", &error);
            if let Some(lease) = self.lease.as_ref() {
                lease.record_cleanup_failure(error);
            }
        }
        // A cleanup failure must be recorded before the lease count reaches
        // zero and wakes terminal shutdown.
        drop(self.lease.take());
    }
}

impl<T: ExcelHandleObject> crate::ExcelReturn for Handle<T> {
    type Output = String;

    fn into_excel(self, context: &mut ReturnContext<'_, '_>) -> XllResult<Self::Output> {
        context.publish_existing_handle(|| Ok(self))
    }

    fn invoke(
        context: &mut ReturnContext<'_, '_>,
        operation: impl FnOnce() -> XllResult<Self>,
    ) -> XllResult<String> {
        context.publish_existing_handle(operation)
    }
}

impl<T: ExcelHandleObject> crate::value::MainThreadReturn for Handle<T> {}

struct HandleLeaseState {
    active: AtomicUsize,
    wait_lock: Mutex<()>,
    idle: Condvar,
    cleanup_failure: Mutex<Option<XllError>>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
    #[cfg(test)]
    before_idle_wait_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl HandleLeaseState {
    fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
            cleanup_failure: Mutex::new(None),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: Mutex::new(None),
            #[cfg(test)]
            before_idle_wait_hook: Mutex::new(None),
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        *self.ghost.lock() = Some(ghost);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        if let Some(ghost) = self.ghost.lock().as_ref().cloned() {
            ghost.record_event(event);
        }
    }

    fn acquire(self: &Arc<Self>) -> HandleLease {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .expect("handle lease count cannot overflow");

        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginHandleOperation);

        HandleLease {
            state: Arc::clone(self),
        }
    }

    fn wait_for_idle(&self) {
        let mut guard = self.wait_lock.lock();
        while self.active.load(Ordering::Acquire) != 0 {
            #[cfg(test)]
            if let Some(hook) = self.before_idle_wait_hook.lock().as_ref().cloned() {
                hook();
            }
            self.idle.wait(&mut guard);
        }
    }

    fn record_cleanup_failure(&self, error: XllError) {
        let mut failure = self.cleanup_failure.lock();
        if failure.is_none() {
            *failure = Some(error);
        }
    }

    fn cleanup_result(&self) -> XllResult<()> {
        let failure = self.cleanup_failure.lock();
        match failure.as_ref() {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    #[cfg(test)]
    fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

struct HandleLease {
    state: Arc<HandleLeaseState>,
}

impl HandleLease {
    fn record_cleanup_failure(&self, error: XllError) {
        self.state.record_cleanup_failure(error);
    }
}

impl Clone for HandleLease {
    fn clone(&self) -> Self {
        self.state.acquire()
    }
}

impl Drop for HandleLease {
    fn drop(&mut self) {
        let previous = self
            .state
            .active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .expect("handle lease count remains balanced");

        if previous == 1 {
            let _wait_guard = self.state.wait_lock.lock();
            self.state.idle.notify_all();
        }

        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.state
            .record_ghost_event(crate::shutdown_refinement::GhostEvent::EndHandleOperation);
    }
}

struct HandlePrepareState {
    active: AtomicUsize,
    waiters: AtomicUsize,
    wait_lock: Mutex<()>,
    idle: Condvar,
}

impl HandlePrepareState {
    const fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            waiters: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        }
    }

    fn enter(&self) -> HandlePrepareGuard<'_> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .expect("handle prepare count cannot overflow");

        HandlePrepareGuard { state: self }
    }

    fn wait_for_idle(&self) {
        let mut guard = self.wait_lock.lock();
        self.waiters.fetch_add(1, Ordering::AcqRel);

        while self.active.load(Ordering::Acquire) != 0 {
            self.idle.wait(&mut guard);
        }

        let previous = self.waiters.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
    }
}

struct HandlePrepareGuard<'a> {
    state: &'a HandlePrepareState,
}

impl Drop for HandlePrepareGuard<'_> {
    fn drop(&mut self) {
        let previous = self.state.active.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);

        if previous != 1 || self.state.waiters.load(Ordering::Acquire) == 0 {
            return;
        }

        let _guard = self.state.wait_lock.lock();

        if self.state.active.load(Ordering::Acquire) == 0 {
            self.state.idle.notify_all();
        }
    }
}

struct HandleEntry {
    type_id: TypeId,
    type_name: &'static str,
    value: Arc<dyn Any + Send + Sync>,
}

struct Slot {
    generation: u64,
    entry: Option<HandleEntry>,
}

struct RegistryState {
    slots: Vec<Slot>,
    free: Vec<usize>,
    live: usize,
    closed: bool,
}

pub(crate) struct HandleRegistry {
    session: u64,
    secret: [u8; 32],
    maximum_handles: usize,
    state: RwLock<RegistryState>,
    cleanup_failure: Mutex<Option<XllError>>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
}

struct PendingHandleValue<'a, T>
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
    fn new(registry: &'a HandleRegistry, value: Arc<T>, operation: &'static str) -> Self {
        Self {
            registry,
            value: Some(value),
            operation,
        }
    }

    fn slot(&mut self) -> &mut Option<Arc<T>> {
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

const HANDLE_ENTROPY_DIAGNOSTIC_ID: u64 = 0x4841_4e44_524e_4746;

impl HandleRegistry {
    pub fn try_new(maximum_handles: usize) -> XllResult<Self> {
        Self::try_new_with(maximum_handles, |entropy| getrandom::fill(entropy), true)
    }

    fn try_new_with<E>(
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

    fn from_entropy(maximum_handles: usize, entropy: [u8; 40]) -> Self {
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
    fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        *self.ghost.lock() = Some(ghost);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
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
    fn ghost_handle(&self) -> Option<crate::shutdown_refinement::GhostHandle> {
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

    fn insert_pending<T>(&self, value: &mut Option<Arc<T>>) -> XllResult<String>
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

    fn lookup_handle<T>(&self, token: &str, leases: &Arc<HandleLeaseState>) -> XllResult<Handle<T>>
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
    fn remove<T>(&self, token: &str) -> XllResult<Arc<T>>
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

    fn remove_any(&self, token: &str) -> XllResult<Arc<dyn Any + Send + Sync>> {
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

    fn record_cleanup_result(&self, result: XllResult<()>) {
        if let Err(error) = result {
            let mut failure = self.cleanup_failure.lock();
            if failure.is_none() {
                *failure = Some(error);
            }
        }
    }

    fn cleanup_result(&self) -> XllResult<()> {
        let failure = self.cleanup_failure.lock();
        match failure.as_ref() {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn drop_values(
        &self,
        values: impl IntoIterator<Item = Arc<dyn Any + Send + Sync>>,
        operation: &'static str,
    ) {
        self.record_cleanup_result(drop_handle_values(values, operation));
    }

    fn remove_and_drop(&self, token: &str, operation: &'static str) {
        if let Ok(value) = self.remove_any(token) {
            self.drop_values(std::iter::once(value), operation);
        }
    }

    fn take_values_for_close(&self) -> Vec<Arc<dyn Any + Send + Sync>> {
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

    fn close_with_leases(&self, leases: &HandleLeaseState) -> XllResult<()> {
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

    fn format_token(&self, slot: u32, generation: u64) -> String {
        let tag = encode_tag(&self.authentication_tag(slot, generation));
        format!(
            "xllh:3:{:016x}:{slot:08x}:{generation:016x}:{tag}",
            self.session
        )
    }

    fn parse_token(&self, token: &str) -> XllResult<ParsedToken> {
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

    fn authentication_tag(&self, slot: u32, generation: u64) -> [u8; 16] {
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

fn drop_handle_values(
    values: impl IntoIterator<Item = Arc<dyn Any + Send + Sync>>,
    operation: &'static str,
) -> XllResult<()> {
    let mut failure = None;
    for value in values {
        if catch_unwind(AssertUnwindSafe(|| drop(value))).is_err() {
            crate::diagnostics::report_no_unwind(operation, &XllError::Panic);
            if failure.is_none() {
                failure = Some(XllError::Panic);
            }
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn encode_tag(tag: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(32);
    for byte in tag {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_tag(encoded: &str) -> Option<[u8; 16]> {
    if encoded.len() != 32 {
        return None;
    }
    let mut tag = [0_u8; 16];
    for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
        tag[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Some(tag)
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

struct Topic {
    token: String,
    #[cfg(any(target_os = "windows", test))]
    server_generation: Option<u64>,
    excel_topic: Option<HandleTopicOwner>,
    #[cfg(any(target_os = "windows", test))]
    excel_topic_committed: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct HandleTopicOwner {
    server_generation: u64,
    topic_id: i32,
}

#[cfg(any(target_os = "windows", test))]
pub(crate) struct HandleConnection {
    runtime: Weak<HandleRuntime>,
    owner: HandleTopicOwner,
    key: String,
    token: String,
    created: bool,
    finished: bool,
}

#[cfg(any(target_os = "windows", test))]
impl HandleConnection {
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub(crate) fn commit(mut self) -> XllResult<()> {
        if self.finished {
            return Ok(());
        }
        if self.created {
            let runtime = self.runtime.upgrade().ok_or(XllError::Closing)?;
            runtime.commit_connection(self.owner, &self.key)?;
        }
        self.finished = true;
        Ok(())
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.created
            && let Some(runtime) = self.runtime.upgrade()
        {
            runtime.rollback_connection(self.owner, &self.key);
        }
    }
}

#[cfg(any(target_os = "windows", test))]
impl Drop for HandleConnection {
    fn drop(&mut self) {
        self.finish();
    }
}

struct TopicState {
    by_key: HashMap<String, Topic>,
    by_excel_id: HashMap<HandleTopicOwner, String>,
    initializing: HashMap<String, Arc<Initialization>>,
    generation: u64,
    closed: bool,
}

impl Default for TopicState {
    fn default() -> Self {
        Self {
            by_key: HashMap::new(),
            by_excel_id: HashMap::new(),
            initializing: HashMap::new(),
            generation: 1,
            closed: false,
        }
    }
}

struct Initialization {
    owner: ThreadId,
    owner_done: AtomicBool,
    completed: Condvar,
}

enum PrepareDecision {
    Existing {
        token: String,
        generation: u64,
    },
    Initialize {
        initialization: Arc<Initialization>,
        generation: u64,
    },
}

thread_local! {
    static ACTIVE_HANDLE_INITIALIZATION_DEPTH: Cell<u32> = const { Cell::new(0) };
}

struct HandleInitializationGuard;

impl HandleInitializationGuard {
    fn enter() -> XllResult<Self> {
        ACTIVE_HANDLE_INITIALIZATION_DEPTH.with(|depth| {
            if depth.get() != 0 {
                return Err(XllError::ReentrantCall);
            }
            depth.set(1);
            Ok(Self)
        })
    }
}

impl Drop for HandleInitializationGuard {
    fn drop(&mut self) {
        ACTIVE_HANDLE_INITIALIZATION_DEPTH.with(|depth| {
            debug_assert_eq!(depth.get(), 1);
            depth.set(0);
        });
    }
}

/// Runtime-owned handle topics. Application code never inserts or removes
/// entries directly; generated UDF boundaries and Excel RTD callbacks do so.
pub(crate) struct HandleRuntime {
    registry: HandleRegistry,
    topics: Mutex<TopicState>,
    prepares: HandlePrepareState,
    leases: Arc<HandleLeaseState>,
    _module_ingress: Option<&'static crate::ingress::ExportIngress>,
}

impl HandleRuntime {
    #[cfg(test)]
    pub fn try_new(maximum_handles: usize) -> XllResult<Self> {
        Self::try_new_with_ingress(maximum_handles, None)
    }

    pub(crate) fn try_new_with_ingress(
        maximum_handles: usize,
        module_ingress: Option<&'static crate::ingress::ExportIngress>,
    ) -> XllResult<Self> {
        Ok(Self {
            registry: HandleRegistry::try_new(maximum_handles)?,
            topics: Mutex::new(TopicState::default()),
            prepares: HandlePrepareState::new(),
            leases: Arc::new(HandleLeaseState::new()),
            _module_ingress: module_ingress,
        })
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        self.registry.set_ghost(Arc::clone(&ghost));
        self.leases.set_ghost(ghost);
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn begin_rtd_operation(&self) -> XllResult<RtdOperationGuard> {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        let ghost = self.registry.ghost_handle();

        let ingress_guard = if let Some(ingress) = self._module_ingress {
            let (guard, accepted) = ingress.enter_with(|| {
                #[cfg(any(test, feature = "shutdown-refinement"))]
                if let Some(ghost) = ghost.as_ref() {
                    ghost.record_event(crate::shutdown_refinement::GhostEvent::BeginRtdOperation);
                }
            });
            if !accepted {
                return Err(XllError::Closing);
            }
            Some(guard)
        } else {
            None
        };

        Ok(RtdOperationGuard {
            _ingress_guard: ingress_guard,
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost,
        })
    }

    #[cfg(test)]
    #[must_use]
    pub fn new(maximum_handles: usize) -> Self {
        Self::try_new(maximum_handles).expect("test host provides an OS CSPRNG")
    }

    #[cfg(test)]
    pub fn prepare<T>(
        &self,
        key: String,
        create: impl FnOnce() -> XllResult<Arc<T>>,
    ) -> XllResult<(String, bool)>
    where
        T: ExcelHandleObject,
    {
        self.prepare_observed(key, create, |_, _| Ok(()))
    }

    fn observe_existing(
        &self,
        key: &str,
        token: String,
        generation: u64,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
    ) -> XllResult<(String, bool)> {
        observe(key, &token)?;

        let topics = self.topics.lock();

        if topics.closed || topics.generation != generation {
            return Err(XllError::Closing);
        }

        if !topics
            .by_key
            .get(key)
            .is_some_and(|topic| topic.token == token)
        {
            return Err(XllError::StaleHandle);
        }

        Ok((token, false))
    }

    pub(crate) fn prepare_observed<T>(
        &self,
        key: String,
        create: impl FnOnce() -> XllResult<Arc<T>>,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
    ) -> XllResult<(String, bool)>
    where
        T: ExcelHandleObject,
    {
        let _active_initialization = HandleInitializationGuard::enter()?;
        let _prepare = self.prepares.enter();
        let _handle_operation = self.leases.acquire();

        let decision = loop {
            let mut topics = self.topics.lock();

            if topics.closed {
                return Err(XllError::Closing);
            }

            //
            // 1. A cold publication for this key is still in progress.
            //
            if let Some(initialization) = topics.initializing.get(&key).cloned() {
                if initialization.owner == std::thread::current().id() {
                    return Err(XllError::ReentrantCall);
                }

                initialization.completed.wait(&mut topics);
                continue;
            }

            //
            // 2. No initialization is in flight, so a visible topic is committed
            //    enough to use as the memoized value.
            //
            if let Some(topic) = topics.by_key.get(&key) {
                break PrepareDecision::Existing {
                    token: topic.token.clone(),
                    generation: topics.generation,
                };
            }

            //
            // 3. Real miss. Become the single-flight owner.
            //
            let initialization = Arc::new(Initialization {
                owner: std::thread::current().id(),
                owner_done: AtomicBool::new(false),
                completed: Condvar::new(),
            });

            topics
                .initializing
                .insert(key.clone(), Arc::clone(&initialization));

            break PrepareDecision::Initialize {
                initialization,
                generation: topics.generation,
            };
        };

        let (initialization, generation) = match decision {
            PrepareDecision::Existing { token, generation } => {
                return self.observe_existing(&key, token, generation, observe);
            }

            PrepareDecision::Initialize {
                initialization,
                generation,
            } => (initialization, generation),
        };

        let initializing = scopeguard::guard(
            (&self.topics, key.as_str(), Arc::clone(&initialization)),
            |(topics, key, owned)| {
                let mut topics = topics.lock();
                let removed = topics
                    .initializing
                    .get(key)
                    .filter(|current| Arc::ptr_eq(current, &owned))
                    .is_some()
                    .then(|| topics.initializing.remove(key))
                    .flatten();
                drop(topics);
                owned.owner_done.store(true, Ordering::Release);
                if let Some(initialization) = removed {
                    initialization.completed.notify_all();
                } else {
                    owned.completed.notify_all();
                }
            },
        );

        //
        // Cold path: no existing topic, invoke the factory.
        //
        let value = match create() {
            Ok(value) => value,
            Err(error) => {
                return Err(error);
            }
        };
        let mut value =
            PendingHandleValue::new(&self.registry, value, "unpublished handle formula value");

        let token = self.registry.insert_pending(value.slot())?;
        let unpublished = scopeguard::guard(
            (&self.registry, &self.topics, key.as_str(), token.as_str()),
            |(registry, topics, key, token)| {
                let mut topics = topics.lock();
                if let Some(topic) = topics.by_key.get(key).filter(|topic| topic.token == token) {
                    if let Some(owner) = topic.excel_topic {
                        topics.by_excel_id.remove(&owner);
                    }
                    topics.by_key.remove(key);
                }
                drop(topics);
                registry.remove_and_drop(token, "handle publication rollback");
            },
        );

        let mut topics = self.topics.lock();
        if topics.closed || topics.generation != generation {
            return Err(XllError::Closing);
        }
        topics.by_key.insert(
            key.clone(),
            Topic {
                token: token.clone(),
                #[cfg(any(target_os = "windows", test))]
                server_generation: None,
                excel_topic: None,
                #[cfg(any(target_os = "windows", test))]
                excel_topic_committed: false,
            },
        );
        drop(topics);

        {
            let topics = self.topics.lock();
            if topics.closed || topics.generation != generation {
                return Err(XllError::Closing);
            }
        }
        observe(&key, &token)?;

        let topics = self.topics.lock();
        if topics.closed || topics.generation != generation {
            return Err(XllError::Closing);
        }
        if !topics
            .by_key
            .get(&key)
            .is_some_and(|topic| topic.token == token)
        {
            return Err(XllError::StaleHandle);
        }
        let _ = scopeguard::ScopeGuard::into_inner(unpublished);
        drop(topics);
        drop(initializing);
        Ok((token, true))
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn claim_server(&self, key: &str, server_generation: u64) -> XllResult<()> {
        let mut topics = self.topics.lock();
        if topics.closed {
            return Err(XllError::Closing);
        }
        let topic = topics.by_key.get_mut(key).ok_or(XllError::StaleHandle)?;
        if topic
            .server_generation
            .is_some_and(|existing| existing != server_generation)
        {
            return Err(XllError::InvalidHandle);
        }
        topic.server_generation = Some(server_generation);
        Ok(())
    }

    #[cfg(test)]
    pub fn connect(
        &self,
        server_generation: u64,
        excel_topic_id: i32,
        key: &str,
    ) -> XllResult<String> {
        let owner = HandleTopicOwner {
            server_generation,
            topic_id: excel_topic_id,
        };
        let (token, created) = self.connect_inner(server_generation, excel_topic_id, key)?;
        if created && let Err(error) = self.commit_connection(owner, key) {
            self.rollback_connection(owner, key);
            return Err(error);
        }
        Ok(token)
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn connect_transaction(
        self: &Arc<Self>,
        server_generation: u64,
        excel_topic_id: i32,
        key: &str,
    ) -> XllResult<HandleConnection> {
        let owner = HandleTopicOwner {
            server_generation,
            topic_id: excel_topic_id,
        };
        let (token, created) = self.connect_inner(server_generation, excel_topic_id, key)?;
        Ok(HandleConnection {
            runtime: Arc::downgrade(self),
            owner,
            key: key.to_owned(),
            token,
            created,
            finished: false,
        })
    }

    #[cfg(any(target_os = "windows", test))]
    fn connect_inner(
        &self,
        server_generation: u64,
        excel_topic_id: i32,
        key: &str,
    ) -> XllResult<(String, bool)> {
        let owner = HandleTopicOwner {
            server_generation,
            topic_id: excel_topic_id,
        };
        let mut topics = self.topics.lock();
        if topics.closed {
            return Err(XllError::Closing);
        }
        if topics
            .by_excel_id
            .get(&owner)
            .is_some_and(|existing| existing != key)
        {
            return Err(XllError::InvalidHandle);
        }
        let (token, created) = {
            let topic = topics.by_key.get_mut(key).ok_or(XllError::StaleHandle)?;
            if topic
                .server_generation
                .is_some_and(|existing| existing != server_generation)
            {
                return Err(XllError::InvalidHandle);
            }
            topic.server_generation = Some(server_generation);
            let created = if let Some(existing) = topic.excel_topic {
                if existing != owner {
                    return Err(XllError::InvalidHandle);
                }
                if !topic.excel_topic_committed {
                    return Err(XllError::Overloaded);
                }
                false
            } else {
                topic.excel_topic = Some(owner);
                topic.excel_topic_committed = false;
                true
            };
            (topic.token.clone(), created)
        };
        topics.by_excel_id.insert(owner, key.to_owned());
        Ok((token, created))
    }

    #[cfg(any(target_os = "windows", test))]
    fn commit_connection(&self, owner: HandleTopicOwner, key: &str) -> XllResult<()> {
        let mut topics = self.topics.lock();
        if topics.closed {
            return Err(XllError::Closing);
        }
        if topics.by_excel_id.get(&owner).map(String::as_str) != Some(key) {
            return Err(XllError::StaleHandle);
        }
        let topic = topics.by_key.get_mut(key).ok_or(XllError::StaleHandle)?;
        if topic.excel_topic != Some(owner) {
            return Err(XllError::StaleHandle);
        }
        topic.excel_topic_committed = true;
        Ok(())
    }

    #[cfg(any(target_os = "windows", test))]
    fn rollback_connection(&self, owner: HandleTopicOwner, key: &str) {
        let mut topics = self.topics.lock();
        if topics.by_excel_id.get(&owner).map(String::as_str) != Some(key)
            || !topics.by_key.get(key).is_some_and(|topic| {
                topic.excel_topic == Some(owner) && !topic.excel_topic_committed
            })
        {
            return;
        }
        topics.by_excel_id.remove(&owner);
        if let Some(topic) = topics.by_key.get_mut(key) {
            // The formula already owns the object and token. Roll back only
            // the COM topic assignment so a failed value write can be retried.
            topic.excel_topic = None;
            topic.excel_topic_committed = false;
        }
    }

    #[cfg(test)]
    pub fn rollback(&self, key: &str) {
        let token = {
            let mut topics = self.topics.lock();
            let Some(topic) = topics.by_key.remove(key) else {
                return;
            };
            if let Some(owner) = topic.excel_topic {
                topics.by_excel_id.remove(&owner);
            }
            Some(topic.token)
        };
        if let Some(token) = token {
            self.registry
                .remove_and_drop(&token, "handle topic rollback");
        }
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn disconnect(&self, server_generation: u64, excel_topic_id: i32) {
        let owner = HandleTopicOwner {
            server_generation,
            topic_id: excel_topic_id,
        };
        let token = {
            let mut topics = self.topics.lock();
            let Some(key) = topics.by_excel_id.remove(&owner) else {
                return;
            };
            topics.by_key.remove(&key).map(|topic| topic.token)
        };
        if let Some(token) = token {
            self.registry
                .remove_and_drop(&token, "handle topic disconnect");
        }
    }

    pub fn lookup<T>(&self, token: &str) -> XllResult<Handle<T>>
    where
        T: ExcelHandleObject,
    {
        self.registry.lookup_handle(token, &self.leases)
    }

    pub fn close(&self) -> XllResult<()> {
        let initializations = {
            let mut topics = self.topics.lock();

            topics.closed = true;
            topics.generation = topics.generation.wrapping_add(1);
            topics.by_key.clear();
            topics.by_excel_id.clear();

            topics
                .initializing
                .drain()
                .map(|(_, value)| value)
                .collect::<Vec<_>>()
        };

        //
        // Wake cold-path waiters.
        //
        for initialization in &initializations {
            initialization.completed.notify_all();
        }

        //
        // Preserve the current cold-owner synchronization.
        //
        for initialization in initializations {
            let mut topics = self.topics.lock();

            while !initialization.owner_done.load(Ordering::Acquire) {
                initialization.completed.wait(&mut topics);
            }
        }

        //
        // warm prepares are no longer represented in `initializing`.
        // Wait for every prepare_observed call that entered before or during
        // the close transition to leave before closing the registry.
        //
        self.prepares.wait_for_idle();

        self.registry.close_with_leases(&self.leases)
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn terminate_topics(&self, server_generation: u64) {
        let tokens = {
            let mut topics = self.topics.lock();
            let keys = topics
                .by_key
                .iter()
                .filter(|(_, topic)| topic.server_generation == Some(server_generation))
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| {
                    let topic = topics.by_key.remove(&key)?;
                    if let Some(owner) = topic.excel_topic {
                        topics.by_excel_id.remove(&owner);
                    }
                    Some(topic.token)
                })
                .collect::<Vec<_>>()
        };
        for token in tokens {
            self.registry
                .remove_and_drop(&token, "handle RTD termination");
        }
    }

    pub fn terminate_all_topics(&self) {
        let tokens = {
            let mut topics = self.topics.lock();
            let tokens = topics
                .by_key
                .drain()
                .map(|(_, topic)| topic.token)
                .collect::<Vec<_>>();
            topics.by_excel_id.clear();
            tokens
        };
        for token in tokens {
            self.registry
                .remove_and_drop(&token, "handle RTD termination");
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.registry.len()
    }
}

#[cfg(target_os = "windows")]
pub(crate) struct RtdOperationGuard {
    _ingress_guard: Option<crate::ingress::ExportCallGuard<'static>>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: Option<crate::shutdown_refinement::GhostHandle>,
}

#[cfg(target_os = "windows")]
impl Drop for RtdOperationGuard {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        if let Some(ghost) = self.ghost.as_ref() {
            ghost.record_event(crate::shutdown_refinement::GhostEvent::EndRtdOperation);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FormulaCaller {
    sheet_id: xlfn_sys::IDSHEET,
    row: i32,
    column: i32,
}

pub(crate) fn format_formula_topic_key(
    caller: FormulaCaller,
    udf_id: &'static str,
    argument_digest: &[u8; 32],
) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    // 20 digits for a 64-bit IDSHEET, 11 for each i32 coordinate, four
    // separators, and the 64-character digest. This upper bound keeps the
    // complete key in one allocation on both supported pointer widths.
    const NUMERIC_KEY_CAPACITY: usize = 20 + 11 + 11 + 4;
    let mut result =
        String::with_capacity(NUMERIC_KEY_CAPACITY + udf_id.len() + argument_digest.len() * 2);

    write!(
        &mut result,
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}",
        caller.sheet_id, caller.row, caller.column, udf_id,
    )
    .expect("writing to String cannot fail");

    for byte in argument_digest {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }

    result
}

pub(crate) fn resolve_formula_caller(
    callbacks: &crate::host_callback::HostCallbackSession,
) -> XllResult<FormulaCaller> {
    use xlfn_sys::{XL_SHEET_ID, XL_SHEET_NM, XLF_CALLER, XLTYPE_REF, XLTYPE_SREF};

    // SAFETY: this runs synchronously on the generated main-thread UDF boundary.
    let (status, mut caller) = unsafe {
        callbacks
            .call(XLF_CALLER, &[])
            .map_err(|suppressed| XllError::ExcelApi {
                function: "xlfCaller(suppressed)",
                code: suppressed.status.raw_code(),
            })?
    };
    if status != ExcelCallbackStatus::Success {
        return Err(caller.try_release().err().unwrap_or(XllError::ExcelApi {
            function: "xlfCaller",
            code: status.raw_code(),
        }));
    }
    let (row, column, sheet_id) = {
        let value = caller.borrow()?;
        match value.base_type() {
            XLTYPE_SREF => {
                // SAFETY: the type selects the SRef member.
                let reference = unsafe { value.raw().value.sref };
                if reference.count != 1
                    || reference.reference.rw_first != reference.reference.rw_last
                    || reference.reference.col_first != reference.reference.col_last
                {
                    return Err(XllError::input(
                        "caller",
                        crate::InputError::Malformed(
                            "handle-producing functions require a single-cell caller",
                        ),
                    ));
                }
                (
                    reference.reference.rw_first,
                    reference.reference.col_first,
                    None,
                )
            }
            XLTYPE_REF => {
                // SAFETY: the type selects the MRef member.
                let reference = unsafe { value.raw().value.mref };
                // SAFETY: Excel supplies a readable reference table.
                let table = unsafe { reference.references.as_ref() }
                    .ok_or_else(|| XllError::input("caller", crate::InputError::NullPointer))?;
                if table.count != 1 {
                    return Err(XllError::input(
                        "caller",
                        crate::InputError::Malformed(
                            "handle-producing functions require a single-cell caller",
                        ),
                    ));
                }
                let area = table.reftbl[0];
                if area.rw_first != area.rw_last || area.col_first != area.col_last {
                    return Err(XllError::input(
                        "caller",
                        crate::InputError::Malformed(
                            "handle-producing functions require a single-cell caller",
                        ),
                    ));
                }
                (area.rw_first, area.col_first, Some(reference.sheet_id))
            }
            _ => {
                return Err(XllError::input(
                    "caller",
                    crate::InputError::Malformed(
                        "handle-producing functions require a worksheet caller",
                    ),
                ));
            }
        }
    };

    if let Some(sheet_id) = sheet_id {
        caller.try_release()?;
        return Ok(FormulaCaller {
            sheet_id,
            row,
            column,
        });
    }

    let caller_arguments = [caller.raw_pointer()?];
    // SAFETY: caller remains live for the nested xlSheetNm callback.
    let (sheet_status, mut sheet) = unsafe {
        callbacks
            .call(XL_SHEET_NM, &caller_arguments)
            .map_err(|suppressed| XllError::ExcelApi {
                function: "xlSheetNm(suppressed)",
                code: suppressed.status.raw_code(),
            })?
    };
    if sheet_status != ExcelCallbackStatus::Success {
        return Err(sheet.try_release().err().unwrap_or(XllError::ExcelApi {
            function: "xlSheetNm",
            code: sheet_status.raw_code(),
        }));
    }
    // `xlSheetId` accepts the counted external sheet name returned by
    // `xlSheetNm`. The name is only a lookup input; it must never become part
    // of formula identity because workbook and worksheet names can change.
    let sheet_name_argument = [sheet.raw_pointer()?];
    // SAFETY: the counted sheet-name result remains live for this nested
    // callback and the callback session owns its release obligation.
    let (sheet_id_status, mut sheet_id_value) = unsafe {
        callbacks
            .call(XL_SHEET_ID, &sheet_name_argument)
            .map_err(|suppressed| XllError::ExcelApi {
                function: "xlSheetId(suppressed)",
                code: suppressed.status.raw_code(),
            })?
    };
    if sheet_id_status != ExcelCallbackStatus::Success {
        return Err(sheet_id_value
            .try_release()
            .err()
            .unwrap_or(XllError::ExcelApi {
                function: "xlSheetId",
                code: sheet_id_status.raw_code(),
            }));
    }
    let sheet_id = {
        let value = sheet_id_value.borrow()?;
        if value.base_type() != XLTYPE_REF {
            return Err(XllError::input(
                "caller",
                crate::InputError::Malformed("xlSheetId did not return an external reference"),
            ));
        }
        // SAFETY: XLTYPE_REF selects the MRef member, whose sheet_id is the
        // stable Excel worksheet identifier returned by xlSheetId.
        unsafe { value.raw().value.mref.sheet_id }
    };
    sheet_id_value.try_release()?;
    sheet.try_release()?;
    caller.try_release()?;

    Ok(FormulaCaller {
        sheet_id,
        row,
        column,
    })
}

pub(crate) fn formula_topic_key(
    callbacks: &crate::host_callback::HostCallbackSession,
    udf_id: &'static str,
    argument_digest: &[u8; 32],
) -> XllResult<String> {
    let caller = resolve_formula_caller(callbacks)?;
    Ok(format_formula_topic_key(caller, udf_id, argument_digest))
}

struct ParsedToken {
    slot: u32,
    generation: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_production<T>(registry: &HandleRegistry, value: Arc<T>) -> XllResult<String>
    where
        T: Any + Send + Sync + 'static,
    {
        let mut value = Some(value);
        registry.insert_pending(&mut value)
    }

    #[test]
    fn formula_topic_key_uses_the_stable_sheet_identifier() {
        let digest = [0xab_u8; 32];
        let caller = FormulaCaller {
            sheet_id: 17,
            row: 4,
            column: 8,
        };
        let first = format_formula_topic_key(caller, "TEST.CREATE", &digest);
        let recalculated = format_formula_topic_key(caller, "TEST.CREATE", &digest);
        let other_sheet = format_formula_topic_key(
            FormulaCaller {
                sheet_id: 18,
                ..caller
            },
            "TEST.CREATE",
            &digest,
        );

        assert_eq!(first, recalculated);
        assert_ne!(first, other_sheet);
    }

    #[test]
    fn formula_topic_key_changes_with_every_identity_component() {
        let digest = [0x12_u8; 32];
        let caller = FormulaCaller {
            sheet_id: 17,
            row: 4,
            column: 8,
        };
        let base = format_formula_topic_key(caller, "TEST.CREATE", &digest);

        assert_ne!(
            base,
            format_formula_topic_key(
                FormulaCaller {
                    sheet_id: 18,
                    ..caller
                },
                "TEST.CREATE",
                &digest,
            )
        );
        assert_ne!(
            base,
            format_formula_topic_key(FormulaCaller { row: 5, ..caller }, "TEST.CREATE", &digest,)
        );
        assert_ne!(
            base,
            format_formula_topic_key(
                FormulaCaller {
                    column: 9,
                    ..caller
                },
                "TEST.CREATE",
                &digest,
            )
        );
        assert_ne!(
            base,
            format_formula_topic_key(caller, "TEST.OTHER", &digest)
        );
        assert_ne!(
            base,
            format_formula_topic_key(caller, "TEST.CREATE", &[0x13_u8; 32])
        );

        assert!(base.ends_with("1212121212121212121212121212121212121212121212121212121212121212"));
    }

    #[test]
    fn ref_caller_uses_embedded_sheet_id_without_sheet_callbacks() {
        let _callback_guard = crate::test_callback::lock();
        crate::test_callback::install();
        crate::test_callback::reset();
        crate::test_callback::set_formula_caller(crate::test_callback::FormulaCallerKind::Ref);

        let callbacks = crate::host_callback::HostCallbackSession::new();
        let caller = resolve_formula_caller(&callbacks).unwrap();

        assert_eq!(
            caller,
            FormulaCaller {
                sheet_id: 17,
                row: 11,
                column: 3,
            }
        );
        assert_eq!(crate::test_callback::calls_for(xlfn_sys::XLF_CALLER), 1);
        assert_eq!(crate::test_callback::calls_for(xlfn_sys::XL_SHEET_NM), 0);
        assert_eq!(crate::test_callback::calls_for(xlfn_sys::XL_SHEET_ID), 0);
        assert_eq!(crate::test_callback::free_calls(), 1);
    }

    #[test]
    fn sref_caller_keeps_sheet_lookup_fallback() {
        let _callback_guard = crate::test_callback::lock();
        crate::test_callback::install();
        crate::test_callback::reset();
        crate::test_callback::set_formula_caller(crate::test_callback::FormulaCallerKind::SRef);

        let callbacks = crate::host_callback::HostCallbackSession::new();
        let caller = resolve_formula_caller(&callbacks).unwrap();

        assert_eq!(
            caller,
            FormulaCaller {
                sheet_id: 19,
                row: 11,
                column: 3,
            }
        );
        assert_eq!(crate::test_callback::calls_for(xlfn_sys::XLF_CALLER), 1);
        assert_eq!(crate::test_callback::calls_for(xlfn_sys::XL_SHEET_NM), 1);
        assert_eq!(crate::test_callback::calls_for(xlfn_sys::XL_SHEET_ID), 1);
        assert_eq!(crate::test_callback::free_calls(), 3);
    }

    #[test]
    fn generation_prevents_aba_and_lookup_keeps_value_alive() {
        let registry = HandleRegistry::new(4);
        let first = Arc::new(String::from("first"));
        let token = insert_production(&registry, Arc::clone(&first)).unwrap();
        let borrowed = registry.lookup::<String>(&token).unwrap();
        assert_eq!(&*borrowed, "first");

        let removed = registry.remove::<String>(&token).unwrap();
        assert_eq!(&*removed, "first");
        assert!(matches!(
            registry.lookup::<String>(&token),
            Err(XllError::StaleHandle)
        ));

        let replacement =
            insert_production(&registry, Arc::new(String::from("replacement"))).unwrap();
        assert_ne!(token, replacement);
        assert_eq!(&*borrowed, "first");
    }

    #[test]
    fn exhausted_generation_retires_the_slot_permanently() {
        let registry = HandleRegistry::new(2);
        insert_production(&registry, Arc::new(1_u32)).unwrap();
        registry.state.write().slots[0].generation = u64::MAX;
        let final_token = registry.format_token(0, u64::MAX);
        assert_eq!(*registry.remove::<u32>(&final_token).unwrap(), 1);
        assert!(registry.state.read().free.is_empty());

        let replacement = insert_production(&registry, Arc::new(2_u32)).unwrap();
        assert_eq!(registry.parse_token(&replacement).unwrap().slot, 1);
        assert!(matches!(
            registry.lookup::<u32>(&final_token),
            Err(XllError::StaleHandle)
        ));
    }

    #[test]
    fn corruption_and_cross_session_tokens_are_rejected() {
        let first = HandleRegistry::new(2);
        let second = HandleRegistry::new(2);
        let token = insert_production(&first, Arc::new(1_u32)).unwrap();
        let fields = token.split(':').collect::<Vec<_>>();
        assert_eq!(fields[1], "3");
        assert_eq!(fields[5].len(), 32);
        let mut corrupted = token.clone();
        let last = corrupted.pop().unwrap();
        corrupted.push(if last == '0' { '1' } else { '0' });
        assert!(first.lookup::<u32>(&corrupted).is_err());
        let forged = format!(
            "xllh:3:{}:{}:{}:{}",
            fields[2],
            fields[3],
            fields[4],
            "0".repeat(32)
        );
        assert!(first.lookup::<u32>(&forged).is_err());
        assert!(second.lookup::<u32>(&token).is_err());
    }

    #[test]
    fn csprng_failure_is_a_stable_initialization_error_not_a_panic() {
        let error = HandleRegistry::try_new_with(2, |_| Err("injected CSPRNG failure"), false)
            .err()
            .expect("injected entropy failure is returned");
        assert!(matches!(
            error,
            XllError::Internal {
                diagnostic_id: HANDLE_ENTROPY_DIAGNOSTIC_ID
            }
        ));
    }

    #[test]
    fn close_invalidates_tokens_but_existing_arcs_survive() {
        let registry = HandleRegistry::new(2);
        let token = insert_production(&registry, Arc::new(42_u32)).unwrap();
        let value = registry.lookup::<u32>(&token).unwrap();
        registry.close().unwrap();
        assert!(registry.lookup::<u32>(&token).is_err());
        assert_eq!(*value, 42);
        assert!(matches!(
            insert_production(&registry, Arc::new(7_u32)),
            Err(XllError::Closing)
        ));
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[test]
    #[ignore = "run in the dedicated Shuttle test step"]
    fn shuttle_insert_racing_close_never_leaves_a_live_handle() {
        shuttle::check_random(
            || {
                let registry = shuttle::sync::Arc::new(HandleRegistry::new(2));
                let inserting = shuttle::sync::Arc::clone(&registry);
                let worker = shuttle::thread::spawn(move || {
                    shuttle::thread::yield_now();
                    insert_production(&inserting, Arc::new(42_u32))
                });

                shuttle::thread::yield_now();
                registry.close().unwrap();
                let result = worker.join().expect("insertion thread panicked");

                assert_eq!(registry.len(), 0);
                match result {
                    Ok(token) => assert!(matches!(
                        registry.lookup::<u32>(&token),
                        Err(XllError::Closing)
                    )),
                    Err(error) => assert!(matches!(error, XllError::Closing)),
                }
            },
            100,
        );
    }

    #[test]
    fn wrong_remove_type_does_not_consume_handle() {
        let registry = HandleRegistry::new(2);
        let token = insert_production(&registry, Arc::new(42_u32)).unwrap();
        assert!(matches!(
            registry.remove::<String>(&token),
            Err(XllError::InvalidHandle)
        ));
        assert_eq!(*registry.lookup::<u32>(&token).unwrap(), 42);
    }

    #[test]
    fn close_drops_values_outside_registry_lock() {
        struct ReenterOnDrop {
            registry: Arc<HandleRegistry>,
        }
        impl Drop for ReenterOnDrop {
            fn drop(&mut self) {
                assert!(matches!(
                    insert_production(&self.registry, Arc::new(1_u32)),
                    Err(XllError::Closing)
                ));
            }
        }

        let registry = Arc::new(HandleRegistry::new(2));
        insert_production(
            &registry,
            Arc::new(ReenterOnDrop {
                registry: Arc::clone(&registry),
            }),
        )
        .unwrap();
        registry.close().unwrap();
    }

    #[test]
    fn close_contains_panicking_destructors_and_continues_dropping() {
        struct PanicOnDrop;
        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                panic!("injected handle destructor panic");
            }
        }

        struct CountOnDrop(Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for CountOnDrop {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let registry = HandleRegistry::new(2);
        insert_production(&registry, Arc::new(PanicOnDrop)).unwrap();
        insert_production(&registry, Arc::new(CountOnDrop(Arc::clone(&drops)))).unwrap();

        assert!(matches!(registry.close(), Err(XllError::Panic)));
        assert_eq!(registry.len(), 0);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn escaped_handle_destructor_panic_poisons_terminal_close() {
        struct PanicOnDrop;
        impl ExcelHandleObject for PanicOnDrop {}
        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                panic!("injected escaped handle destructor panic");
            }
        }

        let runtime = HandleRuntime::new(2);
        let (token, _) = runtime
            .prepare("escaped-panic".to_owned(), || Ok(Arc::new(PanicOnDrop)))
            .unwrap();
        let escaped = runtime.lookup::<PanicOnDrop>(&token).unwrap();

        // Remove the formula-owned registry root first. The escaped Handle now
        // owns the final Arc and must contain its destructor panic itself.
        runtime.rollback("escaped-panic");
        drop(escaped);

        assert!(matches!(runtime.close(), Err(XllError::Panic)));
    }

    #[derive(Debug)]
    struct DataRecord(u32);

    impl ExcelHandleObject for DataRecord {}

    struct SimpleResource;

    impl ExcelHandleObject for SimpleResource {}

    fn token_value(token: &str) -> (Vec<u16>, xlfn_sys::XLOPER12) {
        let mut encoded = Vec::with_capacity(token.encode_utf16().count() + 1);
        encoded.push(token.encode_utf16().count() as u16);
        encoded.extend(token.encode_utf16());
        let raw = xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                string: encoded.as_mut_ptr(),
            },
            xltype: xlfn_sys::XLTYPE_STR,
        };
        (encoded, raw)
    }

    unsafe fn convert_with_context<S, T>(
        runtime: &crate::Runtime<S>,
        argument: &'static str,
        raw: *mut xlfn_sys::XLOPER12,
    ) -> XllResult<T>
    where
        T: for<'call> crate::FromExcel<'call>,
    {
        crate::with_excel_call_scope(|scope| {
            // SAFETY: the test caller keeps the raw value and nested payload live.
            // SAFETY: forwarded from this helper's caller.
            unsafe { crate::argument_from_raw_with_context(scope, runtime, argument, raw) }
        })
    }

    #[test]
    fn repeated_formula_identity_runs_factory_exactly_once() {
        let runtime = HandleRuntime::new(8);
        let calls = AtomicUsize::new(0);

        let (first, created) = runtime
            .prepare("same".to_owned(), || {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(Arc::new(DataRecord(1)))
            })
            .unwrap();
        assert!(created);

        let (second, created) = runtime
            .prepare("same".to_owned(), || {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(Arc::new(DataRecord(2)))
            })
            .unwrap();
        assert!(!created);
        assert_eq!(first, second);
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        assert_eq!(runtime.lookup::<DataRecord>(&first).unwrap().0, 1);

        runtime.connect(1, 41, "same").unwrap();
        runtime.disconnect(1, 41);
        assert_eq!(runtime.len(), 0);
        assert!(matches!(
            runtime.lookup::<DataRecord>(&first),
            Err(XllError::StaleHandle)
        ));
    }

    #[test]
    fn explicit_handle_argument_conversion_resolves_a_typed_token() {
        let runtime: crate::Runtime<()> = crate::Runtime::new();
        let handles = runtime.handles().unwrap();
        let (token, _) = handles
            .prepare("argument".to_owned(), || Ok(Arc::new(DataRecord(19))))
            .unwrap();
        let (_encoded, mut raw) = token_value(&token);

        type DataRecordHandle = Handle<DataRecord>;
        // SAFETY: `raw` and its counted UTF-16 storage remain live for conversion.
        let resolved: DataRecordHandle =
            unsafe { convert_with_context(&runtime, "dataset", &mut raw) }.unwrap();
        assert_eq!(resolved.0, 19);
    }

    #[test]
    fn generic_handle_conversion_rejects_wrong_stale_foreign_and_tampered_tokens() {
        let runtime: crate::Runtime<()> = crate::Runtime::new();
        let handles = runtime.handles().unwrap();
        let (token, _) = handles
            .prepare("argument-errors".to_owned(), || {
                Ok(Arc::new(DataRecord(23)))
            })
            .unwrap();
        handles.connect(1, 91, "argument-errors").unwrap();

        let (_wrong_encoded, mut wrong_raw) = token_value(&token);
        // SAFETY: `wrong_raw` and its counted UTF-16 storage remain live for conversion.
        let wrong = unsafe {
            convert_with_context::<_, Handle<SimpleResource>>(&runtime, "curve", &mut wrong_raw)
        };
        assert!(matches!(wrong, Err(XllError::InvalidHandle)));

        let foreign_runtime: crate::Runtime<()> = crate::Runtime::new();
        let (_foreign_encoded, mut foreign_raw) = token_value(&token);
        // SAFETY: `foreign_raw` and its counted UTF-16 storage remain live for conversion.
        let foreign = unsafe {
            convert_with_context::<_, Handle<DataRecord>>(
                &foreign_runtime,
                "dataset",
                &mut foreign_raw,
            )
        };
        assert!(matches!(foreign, Err(XllError::InvalidHandle)));

        let mut tampered = token.clone();
        let last = tampered.pop().unwrap();
        tampered.push(if last == '0' { '1' } else { '0' });
        let (_tampered_encoded, mut tampered_raw) = token_value(&tampered);
        // SAFETY: `tampered_raw` and its counted UTF-16 storage remain live for conversion.
        let tampered = unsafe {
            convert_with_context::<_, Handle<DataRecord>>(&runtime, "dataset", &mut tampered_raw)
        };
        assert!(matches!(tampered, Err(XllError::InvalidHandle)));

        handles.disconnect(1, 91);
        let (_stale_encoded, mut stale_raw) = token_value(&token);
        // SAFETY: `stale_raw` and its counted UTF-16 storage remain live for conversion.
        let stale = unsafe {
            convert_with_context::<_, Handle<DataRecord>>(&runtime, "dataset", &mut stale_raw)
        };
        assert!(matches!(stale, Err(XllError::StaleHandle)));
    }

    #[test]
    fn optional_handle_conversion_preserves_blank_and_missing_policy() {
        let runtime: crate::Runtime<()> = crate::Runtime::new();
        let mut blank = xlfn_sys::XLOPER12::nil();
        let mut missing = xlfn_sys::XLOPER12::missing();
        // SAFETY: `blank` remains live for the duration of conversion.
        let blank_value = unsafe {
            convert_with_context::<_, Option<Handle<DataRecord>>>(&runtime, "dataset", &mut blank)
        }
        .unwrap();
        // SAFETY: `missing` remains live for the duration of conversion.
        let missing_value = unsafe {
            convert_with_context::<_, Option<Handle<DataRecord>>>(&runtime, "dataset", &mut missing)
        }
        .unwrap();
        assert!(blank_value.is_none());
        assert!(missing_value.is_none());

        // SAFETY: `blank` remains live for the duration of conversion.
        let direct_blank = unsafe {
            convert_with_context::<_, Handle<DataRecord>>(&runtime, "dataset", &mut blank)
        };
        assert!(direct_blank.is_err());
    }

    #[test]
    fn existing_handle_publication_creates_an_independent_formula_owner() {
        let runtime = HandleRuntime::new(8);
        let shared = Arc::new(DataRecord(31));
        let (source_token, _) = runtime
            .prepare("source".to_owned(), || Ok(Arc::clone(&shared)))
            .unwrap();
        runtime.connect(1, 1, "source").unwrap();

        let resolved = runtime.lookup::<DataRecord>(&source_token).unwrap();
        let (alias_token, _) = runtime
            .prepare("alias".to_owned(), || Ok(resolved.into_arc()))
            .unwrap();
        runtime.connect(1, 2, "alias").unwrap();
        assert_ne!(source_token, alias_token);

        runtime.disconnect(1, 1);
        assert!(matches!(
            runtime.lookup::<DataRecord>(&source_token),
            Err(XllError::StaleHandle)
        ));
        assert_eq!(runtime.lookup::<DataRecord>(&alias_token).unwrap().0, 31);

        runtime.disconnect(1, 2);
        assert_eq!(runtime.len(), 0);
    }

    #[test]
    fn failed_rtd_connection_rolls_back_pending_object() {
        let runtime = HandleRuntime::new(8);
        runtime
            .prepare("pending".to_owned(), || Ok(Arc::new(DataRecord(1))))
            .unwrap();
        runtime.rollback("pending");
        assert_eq!(runtime.len(), 0);
    }

    #[test]
    fn uncalculated_rtd_connection_rolls_back_an_already_connected_topic() {
        let runtime = HandleRuntime::new(8);
        runtime
            .prepare("uncalculated".to_owned(), || Ok(Arc::new(DataRecord(1))))
            .unwrap();
        runtime.connect(1, 9, "uncalculated").unwrap();
        runtime.rollback("uncalculated");
        assert_eq!(runtime.len(), 0);
        runtime.disconnect(1, 9);
        assert_eq!(runtime.len(), 0);
    }

    #[test]
    fn uncommitted_connect_transaction_rolls_back_only_the_excel_connection() {
        let runtime = Arc::new(HandleRuntime::new(8));
        let (token, _) = runtime
            .prepare("transactional".to_owned(), || Ok(Arc::new(DataRecord(1))))
            .unwrap();

        let connection = runtime.connect_transaction(1, 10, "transactional").unwrap();
        assert_eq!(connection.token(), token);
        drop(connection);

        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 1);

        let retry = runtime.connect_transaction(1, 10, "transactional").unwrap();
        assert_eq!(retry.token(), token);
        retry.commit().unwrap();
        runtime.disconnect(1, 10);
        assert_eq!(runtime.len(), 0);
    }

    #[test]
    fn concurrent_handle_connect_rejects_an_uncommitted_assignment() {
        let runtime = Arc::new(HandleRuntime::new(8));
        runtime
            .prepare("concurrent-transaction".to_owned(), || {
                Ok(Arc::new(DataRecord(3)))
            })
            .unwrap();

        let connection = runtime
            .connect_transaction(1, 12, "concurrent-transaction")
            .unwrap();
        assert!(matches!(
            runtime.connect_transaction(1, 12, "concurrent-transaction"),
            Err(XllError::Overloaded)
        ));
        connection.commit().unwrap();

        let repeated = runtime
            .connect_transaction(1, 12, "concurrent-transaction")
            .unwrap();
        repeated.commit().unwrap();
        runtime.disconnect(1, 12);
        assert_eq!(runtime.len(), 0);
    }

    #[test]
    fn failed_repeated_connect_transaction_preserves_existing_connection() {
        let runtime = Arc::new(HandleRuntime::new(8));
        let (token, _) = runtime
            .prepare("existing-transaction".to_owned(), || {
                Ok(Arc::new(DataRecord(2)))
            })
            .unwrap();
        runtime.connect(1, 11, "existing-transaction").unwrap();

        let connection = runtime
            .connect_transaction(1, 11, "existing-transaction")
            .unwrap();
        assert_eq!(connection.token(), token);
        drop(connection);

        assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 2);
        runtime.disconnect(1, 11);
        assert_eq!(runtime.len(), 0);
    }

    #[test]
    fn excel_topic_id_cannot_be_connected_to_two_formula_topics() {
        let runtime = HandleRuntime::new(8);
        runtime
            .prepare("first".to_owned(), || Ok(Arc::new(DataRecord(1))))
            .unwrap();
        runtime
            .prepare("second".to_owned(), || Ok(Arc::new(DataRecord(2))))
            .unwrap();
        runtime.connect(1, 9, "first").unwrap();
        assert!(matches!(
            runtime.connect(1, 9, "second"),
            Err(XllError::InvalidHandle)
        ));
        runtime.disconnect(1, 9);
        assert_eq!(runtime.len(), 1);
    }

    struct CountedDataRecord(Arc<std::sync::atomic::AtomicUsize>);

    impl ExcelHandleObject for CountedDataRecord {}

    impl Drop for CountedDataRecord {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn different_formula_keys_create_distinct_handles() {
        let runtime = HandleRuntime::new(8);
        let (first, _) = runtime
            .prepare("sheet:A1:rate=1".to_owned(), || Ok(Arc::new(DataRecord(1))))
            .unwrap();
        let (second, _) = runtime
            .prepare("sheet:A2:rate=1".to_owned(), || Ok(Arc::new(DataRecord(1))))
            .unwrap();
        let (changed, _) = runtime
            .prepare("sheet:A1:rate=2".to_owned(), || Ok(Arc::new(DataRecord(2))))
            .unwrap();
        assert_ne!(first, second);
        assert_ne!(first, changed);
    }

    #[test]
    fn disconnect_waits_for_an_in_flight_consumer_and_drops_once() {
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let runtime = HandleRuntime::new(8);
        let (token, _) = runtime
            .prepare("sheet:A1".to_owned(), || {
                Ok(Arc::new(CountedDataRecord(Arc::clone(&drops))))
            })
            .unwrap();
        runtime.connect(1, 7, "sheet:A1").unwrap();
        let consumer = runtime.lookup::<CountedDataRecord>(&token).unwrap();
        runtime.disconnect(1, 7);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(consumer);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        runtime.disconnect(1, 7);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn terminate_and_close_release_every_remaining_topic_once() {
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let runtime = HandleRuntime::new(8);
        for key in ["one", "two"] {
            runtime
                .prepare(key.to_owned(), || {
                    Ok(Arc::new(CountedDataRecord(Arc::clone(&drops))))
                })
                .unwrap();
            runtime.claim_server(key, 1).unwrap();
        }
        runtime.terminate_topics(1);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        runtime.close().unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn panicking_factory_does_not_publish_a_topic() {
        let runtime = HandleRuntime::new(8);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = runtime
                .prepare::<DataRecord>("panic".to_owned(), || panic!("injected factory panic"));
        }));
        assert!(panic.is_err());
        assert_eq!(runtime.len(), 0);
    }

    #[test]
    fn same_thread_factory_reentry_returns_an_error_without_waiting() {
        let runtime = HandleRuntime::new(8);
        let (token, created) = runtime
            .prepare("factory-reentry".to_owned(), || {
                let nested =
                    runtime.prepare("factory-reentry".to_owned(), || Ok(Arc::new(DataRecord(2))));
                assert!(matches!(nested, Err(XllError::ReentrantCall)));
                Ok(Arc::new(DataRecord(1)))
            })
            .unwrap();
        assert!(created);
        assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 1);
    }

    #[test]
    fn different_key_factory_reentry_returns_an_error_without_waiting() {
        let runtime = HandleRuntime::new(8);
        let (token, created) = runtime
            .prepare("outer-factory".to_owned(), || {
                let nested =
                    runtime.prepare("inner-factory".to_owned(), || Ok(Arc::new(DataRecord(2))));
                assert!(matches!(nested, Err(XllError::ReentrantCall)));
                Ok(Arc::new(DataRecord(1)))
            })
            .unwrap();
        assert!(created);
        assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 1);
        assert_eq!(runtime.len(), 1);
    }

    #[test]
    fn same_thread_observer_reentry_returns_an_error_without_waiting() {
        let runtime = HandleRuntime::new(8);
        let (token, created) = runtime
            .prepare_observed(
                "observer-reentry".to_owned(),
                || Ok(Arc::new(DataRecord(1))),
                |_, _| {
                    let nested = runtime.prepare("observer-reentry".to_owned(), || {
                        Ok(Arc::new(DataRecord(2)))
                    });
                    assert!(matches!(nested, Err(XllError::ReentrantCall)));
                    Ok(())
                },
            )
            .unwrap();
        assert!(created);
        assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 1);
    }

    #[test]
    fn different_key_observer_reentry_returns_an_error_without_waiting() {
        let runtime = HandleRuntime::new(8);
        let (token, created) = runtime
            .prepare_observed(
                "outer-observer".to_owned(),
                || Ok(Arc::new(DataRecord(1))),
                |_, _| {
                    let nested = runtime
                        .prepare("inner-observer".to_owned(), || Ok(Arc::new(DataRecord(2))));
                    assert!(matches!(nested, Err(XllError::ReentrantCall)));
                    Ok(())
                },
            )
            .unwrap();
        assert!(created);
        assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 1);
        assert_eq!(runtime.len(), 1);
    }

    #[test]
    fn failed_observation_does_not_publish_a_topic_and_allows_retry() {
        let runtime = HandleRuntime::new(8);
        let first = runtime.prepare_observed(
            "observed".to_owned(),
            || Ok(Arc::new(DataRecord(1))),
            |_, _| {
                Err(XllError::ExcelApi {
                    function: "xlfRtd",
                    code: xlfn_sys::XLRET_FAILED,
                })
            },
        );
        assert!(matches!(first, Err(XllError::ExcelApi { .. })));
        assert_eq!(runtime.len(), 0);

        let (token, created) = runtime
            .prepare_observed(
                "observed".to_owned(),
                || Ok(Arc::new(DataRecord(2))),
                |_, _| Ok(()),
            )
            .unwrap();
        assert!(created);
        assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 2);
    }

    #[test]
    fn cache_hit_observe_failure_does_not_invalidate_object() {
        let runtime = HandleRuntime::new(8);
        let (token, created) = runtime
            .prepare_observed(
                "observed-memoized".to_owned(),
                || Ok(Arc::new(DataRecord(1))),
                |_, _| Ok(()),
            )
            .unwrap();
        assert!(created);

        let calls = AtomicUsize::new(0);
        let result = runtime.prepare_observed(
            "observed-memoized".to_owned(),
            || {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(Arc::new(DataRecord(2)))
            },
            |_, _| {
                Err(XllError::ExcelApi {
                    function: "xlfRtd",
                    code: xlfn_sys::XLRET_FAILED,
                })
            },
        );
        assert!(matches!(result, Err(XllError::ExcelApi { .. })));

        // factory was never invoked because cache hit skips it
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        // original object is preserved
        assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 1);
        assert_eq!(runtime.len(), 1);
    }

    #[test]
    fn cache_hit_observe_failure_preserves_existing_topic() {
        let runtime = HandleRuntime::new(8);
        let (token, created) = runtime
            .prepare_observed(
                "observe-retry".to_owned(),
                || Ok(Arc::new(DataRecord(10))),
                |_, _| Ok(()),
            )
            .unwrap();
        assert!(created);

        // Observation failure on warm hit
        let result = runtime.prepare_observed(
            "observe-retry".to_owned(),
            || Ok(Arc::new(DataRecord(20))),
            |_, _| {
                Err(XllError::ExcelApi {
                    function: "xlfRtd",
                    code: xlfn_sys::XLRET_FAILED,
                })
            },
        );
        assert!(matches!(result, Err(XllError::ExcelApi { .. })));

        // Retry with successful observation still reuses the same object
        let (retry_token, created) = runtime
            .prepare_observed(
                "observe-retry".to_owned(),
                || Ok(Arc::new(DataRecord(30))),
                |_, _| Ok(()),
            )
            .unwrap();
        assert!(!created);
        assert_eq!(retry_token, token);
        assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 10);
    }

    #[test]
    fn observation_cannot_commit_a_topic_removed_reentrantly() {
        let runtime = HandleRuntime::new(8);
        let result = runtime.prepare_observed(
            "removed-during-observation".to_owned(),
            || Ok(Arc::new(DataRecord(1))),
            |key, _| {
                runtime.rollback(key);
                Ok(())
            },
        );
        assert!(matches!(result, Err(XllError::StaleHandle)));
        assert_eq!(runtime.len(), 0);
    }

    #[test]
    fn concurrent_waiter_retries_after_observation_failure() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let runtime = Arc::new(HandleRuntime::new(8));
        let (observing_tx, observing_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let first_runtime = Arc::clone(&runtime);
        let first = std::thread::spawn(move || {
            first_runtime.prepare_observed(
                "concurrent-observe".to_owned(),
                || Ok(Arc::new(DataRecord(1))),
                |_, _| {
                    observing_tx.send(()).unwrap();
                    finish_rx.recv().unwrap();
                    Err(XllError::ExcelApi {
                        function: "xlfRtd",
                        code: xlfn_sys::XLRET_FAILED,
                    })
                },
            )
        });
        observing_rx.recv().unwrap();

        let (waiting_tx, waiting_rx) = mpsc::channel();
        let second_runtime = Arc::clone(&runtime);
        let second = std::thread::spawn(move || {
            waiting_tx.send(()).unwrap();
            second_runtime.prepare_observed(
                "concurrent-observe".to_owned(),
                || Ok(Arc::new(DataRecord(2))),
                |_, _| Ok(()),
            )
        });
        waiting_rx.recv().unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let waiter_is_blocked = {
                let topics = runtime.topics.lock();
                topics
                    .initializing
                    .get("concurrent-observe")
                    .is_some_and(|initialization| Arc::strong_count(initialization) >= 2)
            };
            if waiter_is_blocked {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "second prepare did not wait for observation"
            );
            std::thread::yield_now();
        }

        finish_tx.send(()).unwrap();
        assert!(matches!(
            first.join().unwrap(),
            Err(XllError::ExcelApi { .. })
        ));
        let (token, created) = second.join().unwrap().unwrap();
        assert!(created);
        assert_eq!(runtime.lookup::<DataRecord>(&token).unwrap().0, 2);
    }

    #[test]
    fn concurrent_prepare_with_same_key_runs_factory_once() {
        use std::sync::Barrier;
        use std::sync::mpsc;

        let runtime = Arc::new(HandleRuntime::new(8));
        let factory_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let (in_factory_tx, in_factory_rx) = mpsc::channel();
        let barrier = Arc::new(Barrier::new(2));

        let runtime1 = Arc::clone(&runtime);
        let factory_calls1 = Arc::clone(&factory_calls);
        let barrier1 = Arc::clone(&barrier);

        let t1 = std::thread::spawn(move || {
            runtime1
                .prepare("concurrent_key".to_owned(), || {
                    factory_calls1.fetch_add(1, Ordering::SeqCst);
                    in_factory_tx.send(()).unwrap();
                    barrier1.wait();
                    Ok(Arc::new(DataRecord(100)))
                })
                .unwrap()
        });

        in_factory_rx.recv().unwrap();

        let runtime2 = Arc::clone(&runtime);
        let factory_calls2 = Arc::clone(&factory_calls);
        let t2 = std::thread::spawn(move || {
            runtime2
                .prepare("concurrent_key".to_owned(), || {
                    factory_calls2.fetch_add(1, Ordering::SeqCst);
                    Ok(Arc::new(DataRecord(200)))
                })
                .unwrap()
        });

        barrier.wait();

        let res1 = t1.join().unwrap();
        let res2 = t2.join().unwrap();

        // Under memoization, thread 1 creates the topic. Thread 2 waits for
        // thread 1 to finish, then finds the existing topic and reuses it.
        // The factory is invoked exactly once.
        assert_eq!(factory_calls.load(Ordering::SeqCst), 1);
        assert_eq!(res1.0, res2.0);
        assert!(!res2.1);
        assert_eq!(runtime.lookup::<DataRecord>(&res1.0).unwrap().0, 100);
        assert_eq!(runtime.len(), 1);
    }

    #[test]
    fn handle_dependency_chain_propagates_identity_change() {
        let runtime = HandleRuntime::new(16);

        // Upstream: different argument digest → different key → different token
        let (upstream_a, created) = runtime
            .prepare("sheet:A1:CURVE.CREATE:digest_a".to_owned(), || {
                Ok(Arc::new(DataRecord(10)))
            })
            .unwrap();
        assert!(created);

        // Downstream uses upstream token as part of its key, simulating
        // MODEL.CREATE(Handle<Curve>, params). The raw upstream token becomes
        // part of the argument digest, so a different upstream token yields
        // a different downstream key.
        let downstream_key_a = format!("sheet:B1:MODEL.CREATE:{}:params", upstream_a);
        let (downstream_a, created) = runtime
            .prepare(downstream_key_a, || Ok(Arc::new(DataRecord(100))))
            .unwrap();
        assert!(created);

        // Upstream changes (different arguments → different key)
        let (upstream_b, created) = runtime
            .prepare("sheet:A1:CURVE.CREATE:digest_b".to_owned(), || {
                Ok(Arc::new(DataRecord(20)))
            })
            .unwrap();
        assert!(created);
        assert_ne!(upstream_a, upstream_b);

        // Downstream key also changes because the upstream token changed
        let downstream_key_b = format!("sheet:B1:MODEL.CREATE:{}:params", upstream_b);
        let (downstream_b, created) = runtime
            .prepare(downstream_key_b, || Ok(Arc::new(DataRecord(200))))
            .unwrap();
        assert!(created);
        assert_ne!(downstream_a, downstream_b);

        // Both downstream objects are distinct
        assert_eq!(runtime.lookup::<DataRecord>(&downstream_a).unwrap().0, 100);
        assert_eq!(runtime.lookup::<DataRecord>(&downstream_b).unwrap().0, 200);
    }

    #[test]
    fn close_waits_for_all_escaped_handle_leases() {
        use std::sync::mpsc;
        use std::time::Duration;

        let runtime = Arc::new(HandleRuntime::new(8));
        let (token, _) = runtime
            .prepare("leased".to_owned(), || Ok(Arc::new(DataRecord(41))))
            .unwrap();
        runtime.connect(1, 1, "leased").unwrap();

        let first = runtime.lookup::<DataRecord>(&token).unwrap();
        let second = first.clone();
        assert_eq!(runtime.leases.active(), 2);

        let closing_runtime = Arc::clone(&runtime);
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closer = std::thread::spawn(move || {
            closed_tx.send(closing_runtime.close()).unwrap();
        });

        while !runtime.registry.state.read().closed {
            std::thread::yield_now();
        }
        assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());

        drop(first);
        assert_eq!(runtime.leases.active(), 1);
        assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());

        drop(second);
        closed_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        closer.join().unwrap();
        assert_eq!(runtime.leases.active(), 0);
    }

    #[test]
    fn close_wakes_waiter_and_prevents_creator_from_publishing() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let runtime = Arc::new(HandleRuntime::new(8));
        let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (factory_started_tx, factory_started_rx) = mpsc::channel();
        let (release_factory_tx, release_factory_rx) = mpsc::channel();

        let creator_runtime = Arc::clone(&runtime);
        let creator_observed = Arc::clone(&observed);
        let creator = std::thread::spawn(move || {
            creator_runtime.prepare_observed(
                "closing".to_owned(),
                || {
                    factory_started_tx.send(()).unwrap();
                    release_factory_rx.recv().unwrap();
                    Ok(Arc::new(DataRecord(1)))
                },
                |_, _| {
                    creator_observed.store(true, Ordering::Release);
                    Ok(())
                },
            )
        });
        factory_started_rx.recv().unwrap();

        let waiter_runtime = Arc::clone(&runtime);
        let (waiter_done_tx, waiter_done_rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let result =
                waiter_runtime.prepare("closing".to_owned(), || Ok(Arc::new(DataRecord(2))));
            waiter_done_tx.send(result).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let blocked = runtime
                .topics
                .lock()
                .initializing
                .get("closing")
                .is_some_and(|initialization| Arc::strong_count(initialization) >= 4);
            if blocked {
                break;
            }
            assert!(Instant::now() < deadline, "waiter did not block");
            std::thread::yield_now();
        }

        let close_runtime = Arc::clone(&runtime);
        let closer = std::thread::spawn(move || close_runtime.close());
        let deadline = Instant::now() + Duration::from_secs(1);
        while !runtime.topics.lock().closed {
            assert!(
                Instant::now() < deadline,
                "close did not mark runtime closed"
            );
            std::thread::yield_now();
        }
        assert!(matches!(
            waiter_done_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(XllError::Closing)
        ));
        release_factory_tx.send(()).unwrap();
        assert!(matches!(creator.join().unwrap(), Err(XllError::Closing)));
        closer.join().unwrap().unwrap();
        waiter.join().unwrap();
        assert!(!observed.load(Ordering::Acquire));
        assert_eq!(runtime.len(), 0);
    }

    #[test]
    fn nested_handle_in_registry_does_not_deadlock_on_close() {
        struct InnerObj;
        impl ExcelHandleObject for InnerObj {}

        struct OuterObj {
            _inner: Handle<InnerObj>,
        }
        impl ExcelHandleObject for OuterObj {}

        let runtime = Arc::new(HandleRuntime::new(16));
        let (inner_token, _) = runtime
            .prepare("inner".to_string(), || Ok(Arc::new(InnerObj)))
            .unwrap();
        let inner_handle = runtime.lookup::<InnerObj>(&inner_token).unwrap();

        let (outer_token, _) = runtime
            .prepare("outer".to_string(), move || {
                Ok(Arc::new(OuterObj {
                    _inner: inner_handle,
                }))
            })
            .unwrap();
        let outer_handle = runtime.lookup::<OuterObj>(&outer_token).unwrap();

        assert_eq!(runtime.leases.active(), 2);
        drop(outer_handle);
        assert_eq!(runtime.leases.active(), 1);

        runtime.registry.close_with_leases(&runtime.leases).unwrap();
        assert_eq!(runtime.leases.active(), 0);
    }

    #[test]
    fn handle_lease_waiter_is_woken_by_last_release() {
        let leases = Arc::new(HandleLeaseState::new());
        let lease = leases.acquire();

        let waiting = Arc::clone(&leases);
        let (started_tx, started_rx) = std::sync::mpsc::channel();

        let waiter = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            waiting.wait_for_idle();
        });

        started_rx.recv().unwrap();

        drop(lease);

        waiter.join().unwrap();
        assert_eq!(leases.active(), 0);
    }

    #[test]
    fn handle_lease_waiter_synchronization_prevents_lost_wakeup() {
        use std::sync::Barrier;

        let leases = Arc::new(HandleLeaseState::new());
        let lease = leases.acquire();

        let barrier = Arc::new(Barrier::new(2));
        let barrier_hook = Arc::clone(&barrier);
        *leases.before_idle_wait_hook.lock() = Some(Arc::new(move || {
            barrier_hook.wait();
        }));

        let waiting = Arc::clone(&leases);
        let waiter = std::thread::spawn(move || {
            waiting.wait_for_idle();
        });

        barrier.wait();

        drop(lease);

        waiter.join().unwrap();
        assert_eq!(leases.active(), 0);
    }

    #[test]
    fn registry_close_with_leases_waits_for_active_handle_and_blocks_new_lookups() {
        use std::sync::mpsc;
        use std::time::Duration;

        struct TestObj;
        impl ExcelHandleObject for TestObj {}

        let registry = Arc::new(HandleRegistry::new(8));
        let leases = Arc::new(HandleLeaseState::new());

        let (token, _) = registry
            .insert_pending(&mut Some(Arc::new(TestObj)))
            .map(|t| (t, ()))
            .unwrap();

        let handle: Handle<TestObj> = registry.lookup_handle(&token, &leases).unwrap();
        assert_eq!(leases.active(), 1);

        let closing_registry = Arc::clone(&registry);
        let closing_leases = Arc::clone(&leases);
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);

        let closer = std::thread::spawn(move || {
            closed_tx
                .send(closing_registry.close_with_leases(&closing_leases))
                .unwrap();
        });

        while !registry.state.read().closed {
            std::thread::yield_now();
        }

        assert!(matches!(
            registry.lookup_handle::<TestObj>(&token, &leases),
            Err(XllError::Closing)
        ));

        assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());

        drop(handle);

        closed_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        closer.join().unwrap();
        assert_eq!(leases.active(), 0);
    }

    #[test]
    fn warm_hit_does_not_enter_single_flight_initialization() {
        let runtime = HandleRuntime::new(8);

        let (token, created) = runtime
            .prepare_observed(
                "warm-fast".to_owned(),
                || Ok(Arc::new(DataRecord(1))),
                |_, _| Ok(()),
            )
            .unwrap();

        assert!(created);

        let calls = AtomicUsize::new(0);

        let (second, created) = runtime
            .prepare_observed(
                "warm-fast".to_owned(),
                || {
                    calls.fetch_add(1, Ordering::Relaxed);
                    Ok(Arc::new(DataRecord(2)))
                },
                |key, _| {
                    let topics = runtime.topics.lock();

                    assert!(
                        !topics.initializing.contains_key(key),
                        "warm hit must bypass per-key single-flight state",
                    );

                    Ok(())
                },
            )
            .unwrap();

        assert!(!created);
        assert_eq!(token, second);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn close_waits_for_in_flight_warm_observation_before_closing_registry() {
        use std::sync::mpsc;
        use std::time::Duration;

        let runtime = Arc::new(HandleRuntime::new(8));

        runtime
            .prepare_observed(
                "warm-close".to_owned(),
                || Ok(Arc::new(DataRecord(1))),
                |_, _| Ok(()),
            )
            .unwrap();

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let warm_runtime = Arc::clone(&runtime);
        let warm = std::thread::spawn(move || {
            warm_runtime.prepare_observed::<DataRecord>(
                "warm-close".to_owned(),
                || panic!("warm factory must not run"),
                |_, _| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                },
            )
        });

        entered_rx.recv().unwrap();

        let closing_runtime = Arc::clone(&runtime);
        let (closed_tx, closed_rx) = mpsc::channel();

        let closer = std::thread::spawn(move || {
            closed_tx.send(closing_runtime.close()).unwrap();
        });

        while !runtime.topics.lock().closed {
            std::thread::yield_now();
        }

        //
        // close has started, but registry must remain alive while observe executes.
        //
        assert!(!runtime.registry.state.read().closed);

        assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());

        release_tx.send(()).unwrap();

        assert!(matches!(warm.join().unwrap(), Err(XllError::Closing)));

        closed_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();

        closer.join().unwrap();

        assert!(runtime.registry.state.read().closed);
    }
}
