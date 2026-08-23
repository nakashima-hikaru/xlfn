//! Lifecycle and orchestration for the formula-handle registry.
//!
//! `BindingTable` is the only mutable binding store. Each published record
//! owns its `ObjectCell`, while immutable snapshots provide the call-scoped
//! read capability. There is no second object registry, retired queue, or
//! resurrection path to keep in sync.

use super::binding::{BindingReadLease, BindingState, BindingTable};
use super::object::{ObjectCell, ObjectDropReason, ObjectLifetimeTracker, SharedObject};
use super::token::{HandleId, HandleToken, ObjectId, TokenCodec};
use super::{ExcelHandleObject, Handle};
use crate::error::DomainErrorCode;
use crate::{XllError, XllResult};
use std::any::{TypeId, type_name};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

#[cfg(any(test, feature = "unstable"))]
use parking_lot::Mutex;

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

#[derive(Debug)]
pub(crate) struct HandleRegistrySealed {
    _private: (),
}

impl HandleRegistrySealed {
    fn new() -> Self {
        Self { _private: () }
    }
}

pub(crate) struct HandleRegistry {
    pub(super) codec: TokenCodec,
    pub(super) phase: AtomicU8,
    pub(super) bindings: BindingTable,
    pub(super) lifetime: Arc<ObjectLifetimeTracker>,
    next_object_id: AtomicU64,
    #[cfg(any(test, feature = "unstable"))]
    pub(super) ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
}

/// Owns an object until a binding reservation consumes it. If publication
/// fails, dropping this guard releases the object through the same
/// `ObjectCell` destructor path as every other ownership edge.
pub(crate) struct PendingHandleValue {
    value: Option<SharedObject>,
}

impl PendingHandleValue {
    pub(crate) fn new(value: SharedObject) -> Self {
        Self { value: Some(value) }
    }

    pub(crate) fn slot(&mut self) -> &mut Option<SharedObject> {
        &mut self.value
    }
}

impl Drop for PendingHandleValue {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            value.mark_drop_reason(ObjectDropReason::PublicationRollback);
            drop(value);
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
        let lifetime = ObjectLifetimeTracker::new();
        Self {
            codec: TokenCodec::new(session, secret),
            phase: AtomicU8::new(HandleRegistryPhase::Open as u8),
            bindings: BindingTable::new(maximum_bindings),
            lifetime,
            next_object_id: AtomicU64::new(1),
            #[cfg(any(test, feature = "unstable"))]
            ghost: Mutex::new(None),
        }
    }

    #[cfg(any(test, feature = "unstable"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        self.lifetime.set_ghost(Arc::clone(&ghost));
        *self.ghost.lock() = Some(ghost);
    }

    #[cfg(any(test, feature = "unstable"))]
    pub(crate) fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        if let Some(ghost) = self.ghost.lock().as_ref().cloned() {
            ghost.record_event(event);
        }
    }

    #[cfg(all(target_os = "windows", any(test, feature = "unstable")))]
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

    fn allocate_object_id(&self) -> XllResult<ObjectId> {
        self.next_object_id
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map(ObjectId)
            .map_err(|_| XllError::Domain {
                code: DomainErrorCode::Overflow,
            })
    }

    pub(crate) fn new_object<T: Send + Sync + 'static>(&self, value: T) -> XllResult<SharedObject> {
        let object_id = self.allocate_object_id()?;
        ObjectCell::new(object_id, value, Arc::clone(&self.lifetime))
    }

    #[cfg(test)]
    pub fn insert_pending<T>(&self, value: &mut Option<T>) -> XllResult<String>
    where
        T: Send + Sync + 'static,
    {
        let object = self.new_object(value.take().expect("pending handle value is armed"))?;
        let mut object = PendingHandleValue::new(object);
        self.insert_pending_object_with_kind::<T>(object.slot())
            .map(|(token, _binding_id, _object_id, _reused)| token)
    }

    pub(crate) fn insert_pending_object_with_kind<T>(
        &self,
        value: &mut Option<SharedObject>,
    ) -> XllResult<(String, HandleId, ObjectId, bool)>
    where
        T: Send + Sync + 'static,
    {
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let object = value.as_ref().expect("pending handle object is armed");
        self.validate_type::<T>(object)?;
        let reservation = self.bindings.reserve()?;
        let object = value.take().expect("pending handle object is armed");
        let object_id = object.id();
        let (id, reused) = reservation.publish(object);
        #[cfg(any(test, feature = "unstable"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddHandle);
        Ok((self.codec.format(id), id, object_id, reused))
    }

    pub(crate) fn insert_existing_object_binding<T>(
        &self,
        object: SharedObject,
    ) -> XllResult<(String, HandleId, ObjectId, bool)>
    where
        T: Send + Sync + 'static,
    {
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        self.validate_type::<T>(&object)?;
        let object_id = object.id();
        let reservation = self.bindings.reserve()?;
        let (id, reused) = reservation.publish(object);
        #[cfg(any(test, feature = "unstable"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::AddHandle);
        Ok((self.codec.format(id), id, object_id, reused))
    }

    fn validate_type<T: Send + Sync + 'static>(&self, object: &SharedObject) -> XllResult<()> {
        if object.type_id() == TypeId::of::<T>() {
            return Ok(());
        }
        let actual_type = object.type_name();
        let _ = catch_unwind(AssertUnwindSafe(|| {
            tracing::warn!(
                expected_type = type_name::<T>(),
                actual_type,
                "Excel handle object type mismatch"
            );
        }));
        Err(XllError::InvalidHandle)
    }

    #[cfg(test)]
    pub fn lookup<T>(&self, token: &str) -> XllResult<T>
    where
        T: Send + Sync + Clone + 'static,
    {
        let verified = self
            .codec
            .parse(std::ptr::from_ref(self).addr(), HandleToken::new(token))?;
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let lease = BindingReadLease::new(
            self.bindings.published().load(verified.id.slot),
            verified.id,
        )?;
        let record = lease.record();
        if record.state() != BindingState::Live {
            return Err(XllError::StaleHandle);
        }
        let value = record
            .object
            .typed_ptr::<T>()
            .ok_or(XllError::InvalidHandle)?;
        // SAFETY: the read lease owns the object cell containing this value.
        Ok(unsafe { value.as_ref().clone() })
    }

    pub(crate) fn lookup_handle<'call, T>(
        &self,
        _scope: &'call crate::call::CallScope<'call>,
        token: &str,
    ) -> XllResult<Handle<'call, T>>
    where
        T: ExcelHandleObject,
    {
        let verified = self
            .codec
            .parse(std::ptr::from_ref(self).addr(), HandleToken::new(token))?;
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let binding = BindingReadLease::new(
            self.bindings.published().load(verified.id.slot),
            verified.id,
        )?;
        let record = binding.record();
        if record.state() != BindingState::Live {
            return Err(XllError::StaleHandle);
        }
        let Some(value) = record.object.typed_ptr::<T>() else {
            let actual_type = record.object.type_name();
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
        Ok(Handle::new(binding, value))
    }

    #[cfg(test)]
    pub(crate) fn remove<T: Send + Sync + 'static>(&self, token: &str) -> XllResult<()> {
        let verified = self
            .codec
            .parse(std::ptr::from_ref(self).addr(), HandleToken::new(token))?;
        let removal = self.bindings.begin_removal(verified.id)?;
        if removal.object().typed_ptr::<T>().is_none() {
            return Err(XllError::InvalidHandle);
        }
        removal.mark_drop_reason(ObjectDropReason::BindingRemoved);
        removal.commit();
        #[cfg(any(test, feature = "unstable"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        Ok(())
    }

    fn drop_reason(operation: &'static str) -> ObjectDropReason {
        if operation.contains("rollback") {
            ObjectDropReason::PublicationRollback
        } else if operation.contains("close") {
            ObjectDropReason::Shutdown
        } else {
            ObjectDropReason::BindingRemoved
        }
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
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let removal = self.bindings.begin_removal(verified.id)?;
        removal.mark_drop_reason(Self::drop_reason(operation));
        let reusable = removal.commit();
        on_linearized(reusable);
        #[cfg(any(test, feature = "unstable"))]
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        Ok(reusable)
    }

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        self.lifetime.cleanup().result()
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
        self.bindings
            .mark_all_drop_reason(ObjectDropReason::Shutdown);
        let live_bindings = self.bindings.retire_all();
        #[cfg(any(test, feature = "unstable"))]
        for _ in 0..live_bindings {
            self.record_ghost_event(crate::shutdown_refinement::GhostEvent::RemoveHandle);
        }
        usize::try_from(live_bindings).expect("binding count fits in usize")
    }

    pub(crate) fn begin_close(&self) {
        let _ = self.phase.compare_exchange(
            HandleRegistryPhase::Open as u8,
            HandleRegistryPhase::Closing as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(super) fn seal(&self) -> XllResult<HandleRegistrySealed> {
        let previous = self
            .phase
            .swap(HandleRegistryPhase::Closing as u8, Ordering::AcqRel);
        if HandleRegistryPhase::from_raw(previous) == HandleRegistryPhase::Closed {
            self.cleanup_result()?;
            return Ok(HandleRegistrySealed::new());
        }
        self.lifetime.seal();
        self.retire_values_for_seal();
        self.phase
            .store(HandleRegistryPhase::Closed as u8, Ordering::Release);
        self.cleanup_result()?;
        Ok(HandleRegistrySealed::new())
    }

    pub(super) fn finish_quiescence(&self, _sealed: &HandleRegistrySealed) -> XllResult<()> {
        self.lifetime.finish_quiescence()
    }
}
