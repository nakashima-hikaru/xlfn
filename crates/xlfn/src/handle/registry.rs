//! Public-internal facade for the handle registry.
//!
//! Binding publication, object ownership, and write transactions live in
//! sibling modules. This module owns only the registry lifecycle and the
//! orchestration exposed to the rest of the handle subsystem.

use super::binding::{BindingState, BindingTable};
use super::object_store::{
    ErasedObject, HandleCleanupState, LiveObjectRef, ObjectIdentity, ObjectLocator, ObjectRoots,
    ObjectStore,
};
use super::reclamation::CallHandleCapabilities;
use super::token::{HandleId, HandleToken, ObjectId, TokenCodec};
use super::transaction::{RegistryRemovalTxn, RegistryWriteTxn};
use super::{ExcelHandleObject, Handle};
use crate::error::DomainErrorCode;
use crate::{XllError, XllResult};
#[cfg(any(test, feature = "unstable"))]
use parking_lot::Mutex;
use std::any::{TypeId, type_name};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

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
    pub(super) cleanup: Arc<HandleCleanupState>,
    pub(super) objects: Arc<ObjectStore>,
    #[cfg(any(test, feature = "unstable"))]
    pub(super) ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
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
            #[cfg(any(test, feature = "unstable"))]
            ghost: Mutex::new(None),
        }
    }

    #[cfg(any(test, feature = "unstable"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        self.objects.set_ghost(Arc::clone(&ghost));
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
        #[cfg(any(test, feature = "unstable"))]
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
                ObjectRoots::with_binding(),
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
        #[cfg(any(test, feature = "unstable"))]
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
        let objects = self.objects.lock_live();
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
        scope: &'call crate::call::CallScope<'call>,
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
        let capabilities = scope.handle_guard().register(&self.objects)?;
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
            let Some(value) = record.object_ref.resolve::<T>(capabilities.read_guard()) else {
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
            return Ok(Handle::new(object, value, capabilities.pin_context()));
        }
        drop(published_snapshot);

        self.lookup_handle_slow(id, capabilities)
    }

    fn lookup_handle_slow<'call, T>(
        &self,
        id: HandleId,
        capabilities: CallHandleCapabilities<'call>,
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
        let Some(value) = record.object_ref.resolve::<T>(capabilities.read_guard()) else {
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
        Ok(Handle::new(object, value, capabilities.pin_context()))
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
        #[cfg(any(test, feature = "unstable"))]
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
        #[cfg(any(test, feature = "unstable"))]
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
        let values = self.objects.lock_live().take_all();
        self.objects.retire_all(values, "handle registry close");
        #[cfg(any(test, feature = "unstable"))]
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
