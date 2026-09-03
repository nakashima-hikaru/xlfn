//! Lifecycle and orchestration for the formula-handle registry.
//!
//! `BindingTable` is the only mutable binding store. Each published record
//! owns its `ObjectCell`, while immutable snapshots provide the call-scoped
//! read capability. There is no second object registry, retired queue, or
//! resurrection path to keep in sync.

use super::binding::{BindingReadLease, BindingState, BindingTable};
use super::object::{ObjectArena, ObjectBinding};
use super::token::{HandleId, HandleToken, ObjectId, TokenCodec};
use super::{ExcelHandleObject, Handle};
use crate::error::DomainErrorCode;
use crate::{XllError, XllResult};
#[cfg(any(test, feature = "refinement"))]
use parking_lot::Mutex;
use std::any::{TypeId, type_name};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandleRegistryPhase {
    Open = 0,
    Closing = 1,
    Sealing = 2,
    Closed = 3,
}

impl HandleRegistryPhase {
    fn from_raw(raw: u8) -> Self {
        match raw {
            value if value == Self::Open as u8 => Self::Open,
            value if value == Self::Closing as u8 => Self::Closing,
            value if value == Self::Sealing as u8 => Self::Sealing,
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
    pub(super) objects: Box<ObjectArena>,
    next_object_id: AtomicU64,
    #[cfg(any(test, feature = "refinement"))]
    pub(super) trace: Mutex<Option<crate::shutdown_trace::ShutdownTraceHandle>>,
}

/// Owns an object until a binding reservation consumes it. If publication
/// fails, dropping this guard releases the object through the same
/// `ObjectCell` destructor path as every other ownership edge.
pub(crate) struct PendingHandleValue {
    value: Option<ObjectBinding>,
}

impl PendingHandleValue {
    pub(crate) fn new(value: ObjectBinding) -> Self {
        Self { value: Some(value) }
    }

    pub(crate) fn slot(&mut self) -> &mut Option<ObjectBinding> {
        &mut self.value
    }
}

impl Drop for PendingHandleValue {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            drop(value);
        }
    }
}

impl HandleRegistry {
    pub(crate) fn try_new(maximum_bindings: usize) -> XllResult<Self> {
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
                diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_ENTROPY,
            };
            if report_failure {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    tracing::error!(
                        error = ?source,
                        diagnostic_id = crate::diagnostics::id::DiagnosticId::HANDLE_ENTROPY.as_u64(),
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
        Self {
            codec: TokenCodec::new(session, secret),
            phase: AtomicU8::new(HandleRegistryPhase::Open as u8),
            bindings: BindingTable::new(maximum_bindings),
            objects: Box::new(ObjectArena::new()),
            next_object_id: AtomicU64::new(1),
            #[cfg(any(test, feature = "refinement"))]
            trace: Mutex::new(None),
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.objects.set_trace_sink(std::sync::Arc::clone(&trace));
        *self.trace.lock() = Some(trace);
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn record_shutdown_event(&self, event: crate::shutdown_trace::ShutdownEvent) {
        if let Some(trace) = self.trace.lock().as_ref() {
            trace.record(event);
        }
    }

    #[cfg(all(target_os = "windows", any(test, feature = "refinement")))]
    pub(crate) fn trace_handle(&self) -> Option<crate::shutdown_trace::ShutdownTraceHandle> {
        self.trace.lock().clone()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn new(maximum_bindings: usize) -> Self {
        Self::try_new(maximum_bindings).expect("test host provides an OS CSPRNG")
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn len(&self) -> usize {
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
            .map(|sequence| ObjectId::new(self.codec.session, sequence))
            .map_err(|_| XllError::Domain {
                code: DomainErrorCode::Overflow,
            })
    }

    pub(crate) fn new_object<T: Send + Sync + 'static>(
        &self,
        value: T,
    ) -> XllResult<ObjectBinding> {
        let object_id = self.allocate_object_id()?;
        self.objects.insert(object_id, value)
    }

    #[cfg(test)]
    pub(crate) fn insert_pending<T>(&self, value: &mut Option<T>) -> XllResult<String>
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
        value: &mut Option<ObjectBinding>,
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
        if !self.is_open() {
            drop(reservation);
            return Err(XllError::Closing);
        }
        let object = value.take().expect("pending handle object is armed");
        let object_id = object.id();
        let (id, reused) = reservation.publish(object);
        #[cfg(any(test, feature = "refinement"))]
        self.record_shutdown_event(crate::shutdown_trace::ShutdownEvent::AddHandle);
        Ok((self.codec.format(id), id, object_id, reused))
    }

    pub(crate) fn insert_existing_object_binding<T>(
        &self,
        object: ObjectBinding,
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
        if !self.is_open() {
            drop(reservation);
            return Err(XllError::Closing);
        }
        let (id, reused) = reservation.publish(object);
        #[cfg(any(test, feature = "refinement"))]
        self.record_shutdown_event(crate::shutdown_trace::ShutdownEvent::AddHandle);
        Ok((self.codec.format(id), id, object_id, reused))
    }

    fn validate_type<T: Send + Sync + 'static>(&self, object: &ObjectBinding) -> XllResult<()> {
        if object.object().type_id() == TypeId::of::<T>() {
            return Ok(());
        }
        let actual_type = object.object().type_name();
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
    pub(crate) fn lookup<T>(&self, token: &str) -> XllResult<T>
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
            self.bindings.read_domain(),
        )?;
        let record = lease.record();
        if record.state() != BindingState::Live {
            return Err(XllError::StaleHandle);
        }
        let value = record
            .object()
            .typed_projection::<T>()
            .ok_or(XllError::InvalidHandle)?;
        Ok(value.as_ref().clone())
    }

    pub(crate) fn lookup_handle<'call, T>(
        &self,
        scope: &'call crate::call::CallScope<'call>,
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
        scope.enter_handle_domain(self.bindings.read_domain())?;
        let binding = BindingReadLease::new_scoped(
            self.bindings.published().load(verified.id.slot),
            verified.id,
        )?;
        let record = binding.record();
        if record.state() != BindingState::Live {
            return Err(XllError::StaleHandle);
        }
        let Some(value) = record.object().typed_projection::<T>() else {
            let actual_type = record.object().type_name();
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
        if removal.object().typed_projection::<T>().is_none() {
            return Err(XllError::InvalidHandle);
        }
        removal.commit();
        #[cfg(any(test, feature = "refinement"))]
        self.record_shutdown_event(crate::shutdown_trace::ShutdownEvent::RemoveHandle);
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
        if !self.is_open() {
            return Err(XllError::Closing);
        }
        let removal = self.bindings.begin_removal(verified.id)?;
        tracing::trace!(operation, "handle binding retired");
        let reusable = removal.commit();
        on_linearized(reusable);
        #[cfg(any(test, feature = "refinement"))]
        self.record_shutdown_event(crate::shutdown_trace::ShutdownEvent::RemoveHandle);
        Ok(reusable)
    }

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        self.objects.cleanup_result()
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
        let (live_bindings, retired) = self.bindings.retire_all();
        #[cfg(any(test, feature = "refinement"))]
        for _ in 0..live_bindings {
            self.record_shutdown_event(crate::shutdown_trace::ShutdownEvent::RemoveHandle);
        }
        // `retire_all` releases the binding-table write lock before returning;
        // dropping these records here keeps arbitrary user destructors out of
        // that lock's critical section.
        drop(retired);
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
        loop {
            let phase = self.phase();
            match phase {
                HandleRegistryPhase::Closed => {
                    self.cleanup_result()?;
                    return Ok(HandleRegistrySealed::new());
                }
                HandleRegistryPhase::Sealing => return Err(XllError::Closing),
                HandleRegistryPhase::Open | HandleRegistryPhase::Closing => {
                    if self
                        .phase
                        .compare_exchange(
                            phase as u8,
                            HandleRegistryPhase::Sealing as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        break;
                    }
                }
            }
        }
        self.objects.seal();
        self.retire_values_for_seal();
        self.bindings.read_domain().seal();
        self.phase
            .store(HandleRegistryPhase::Closed as u8, Ordering::Release);
        self.cleanup_result()?;
        Ok(HandleRegistrySealed::new())
    }

    pub(super) fn finish_quiescence(&self, _sealed: &HandleRegistrySealed) -> XllResult<()> {
        self.objects.finish_quiescence()
    }
}
