//! Object ownership for formula-owned Excel handles.
//!
//! `HandleStore` is the sole owner of the token registry and object payloads.
//! Formula revision memoization and RTD topic state live in
//! [`super::runtime::FormulaHandleService`]; they use this façade instead of
//! reaching into the registry lifecycle directly.

use super::object::SharedObject;
use super::registry::{HandleRegistry, HandleRegistrySealed, PendingHandleValue};
use super::{ExcelHandleObject, Handle, HandleId, HandleToken, ObjectId, TokenWire};
use crate::XllResult;
use crate::generation::RuntimeGeneration;

/// Owns the binding registry and its `ObjectCell` lifetime state.
///
/// This type deliberately has no formula identity, topic, or RTD state. Those
/// concerns belong to the formula publication service above it.
pub(crate) struct HandleStore {
    pub(super) registry: HandleRegistry,
}

impl HandleStore {
    pub(crate) fn try_new(maximum_bindings: usize) -> XllResult<Self> {
        Ok(Self {
            registry: HandleRegistry::try_new(maximum_bindings)?,
        })
    }

    pub(crate) fn erase<T: ExcelHandleObject>(&self, value: T) -> XllResult<SharedObject> {
        self.registry.new_object(value)
    }

    pub(crate) const fn session(&self) -> u64 {
        self.registry.codec.session
    }

    pub(crate) fn insert_pending<T: ExcelHandleObject>(
        &self,
        value: SharedObject,
    ) -> XllResult<(String, HandleId, ObjectId, bool)> {
        let mut pending = PendingHandleValue::new(value);
        self.registry
            .insert_pending_object_with_kind::<T>(pending.slot())
    }

    pub(crate) fn insert_existing<T: ExcelHandleObject>(
        &self,
        object: SharedObject,
    ) -> XllResult<(String, HandleId, ObjectId, bool)> {
        self.registry.insert_existing_object_binding::<T>(object)
    }

    pub(crate) fn lookup<'call, T: ExcelHandleObject>(
        &self,
        scope: &'call crate::call::CallScope<'call>,
        token: &str,
    ) -> XllResult<Handle<'call, T>> {
        self.registry.lookup_handle(scope, token)
    }

    pub(crate) fn remove_and_drop_with_observer(
        &self,
        token: &str,
        operation: &'static str,
        on_linearized: impl FnOnce(bool),
    ) -> Option<bool> {
        self.registry
            .remove_and_drop_with_observer(token, operation, on_linearized)
    }

    pub(crate) fn refinement_token(&self, token: &str) -> TokenWire {
        let parsed = self
            .registry
            .codec
            .parse(
                std::ptr::from_ref(&self.registry).addr(),
                HandleToken::new(token),
            )
            .expect("H4 trace token must be authenticated");
        TokenWire {
            session: self.registry.codec.session,
            slot: u64::from(parsed.id.slot),
            generation: parsed.id.generation.get(),
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.registry.set_trace_sink(trace);
    }

    #[cfg(all(target_os = "windows", any(test, feature = "refinement")))]
    pub(crate) fn trace_handle(&self) -> Option<crate::shutdown_trace::ShutdownTraceHandle> {
        self.registry.trace_handle()
    }

    pub(crate) fn begin_close(&self) {
        self.registry.begin_close();
    }

    pub(crate) fn seal(&self) -> XllResult<HandleRegistrySealed> {
        self.registry.seal()
    }

    pub(crate) fn finish_quiescence(&self, sealed: &HandleRegistrySealed) -> XllResult<()> {
        self.registry.finish_quiescence(sealed)
    }

    pub(crate) fn quiescent(
        &self,
        sealed: &HandleRegistrySealed,
        generation: Option<RuntimeGeneration>,
    ) -> XllResult<crate::shutdown::HandlesQuiescent> {
        self.finish_quiescence(sealed)?;
        Ok(crate::shutdown::HandlesQuiescent::new(generation))
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.registry.len()
    }
}
