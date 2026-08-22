//! Linear write transactions for the binding and object registries.
//!
//! Both transaction types acquire the binding reservation and live-object
//! lock in the same order. Keeping that protocol in its own module prevents
//! read-side publication code from gaining write capabilities accidentally.

use super::binding::{BindingRemoval, BindingReservation};
use super::object_store::{LiveObjectRef, ObjectRegistry};
use super::reclamation::PublishedObjectPtr;
use super::registry::HandleRegistry;
use super::token::HandleId;
use crate::XllResult;
use parking_lot::MutexGuard;

/// Write-side registry transaction for inserting a binding.
pub(crate) struct RegistryWriteTxn<'a> {
    objects: MutexGuard<'a, ObjectRegistry>,
    binding: BindingReservation<'a>,
}

impl<'a> RegistryWriteTxn<'a> {
    pub(crate) fn reserve(registry: &'a HandleRegistry) -> XllResult<Self> {
        let binding = registry.bindings.reserve()?;
        let objects = registry.objects.live.lock();
        Ok(Self { objects, binding })
    }

    pub(crate) fn objects(&mut self) -> &mut ObjectRegistry {
        &mut self.objects
    }

    pub(crate) fn publish(
        self,
        object: LiveObjectRef,
        object_ref: PublishedObjectPtr,
    ) -> (HandleId, bool) {
        let Self { objects, binding } = self;
        drop(objects);
        binding.publish(object, object_ref)
    }
}

/// Write-side removal transaction.
pub(crate) struct RegistryRemovalTxn<'a> {
    objects: MutexGuard<'a, ObjectRegistry>,
    binding: BindingRemoval<'a>,
}

impl<'a> RegistryRemovalTxn<'a> {
    pub(crate) fn begin(registry: &'a HandleRegistry, id: HandleId) -> XllResult<Self> {
        let binding = registry.bindings.begin_removal(id)?;
        let objects = registry.objects.live.lock();
        Ok(Self { objects, binding })
    }

    pub(crate) fn object(&self) -> LiveObjectRef {
        self.binding.object()
    }

    pub(crate) fn objects(&mut self) -> &mut ObjectRegistry {
        &mut self.objects
    }

    pub(crate) fn commit(self) -> bool {
        let Self { objects, binding } = self;
        drop(objects);
        binding.commit()
    }
}
