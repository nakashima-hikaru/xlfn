//! One cold constructor/ABI dispatch boundary chooses the add-in's concrete server type.
//! No borrowed runtime state is retained here; rollback can leave the descriptor inert.
use super::server::ACTIVE_SERVER;
use crate::sync::Mutex;
use crate::{RtdSubscription, XllError, XllResult};
use std::any::TypeId;
use std::ffi::c_void;

#[derive(Clone, Copy)]
struct ServerType {
    identity: TypeId,
    #[cfg(test)]
    discard: fn(usize, bool),
    class_factory: unsafe fn(*const c_void, *const c_void, *mut *mut c_void) -> i32,
    #[cfg(feature = "handles")]
    shutdown: fn(&crate::formula_lifetime::FormulaLifetimeBackend) -> XllResult<()>,
}

impl ServerType {
    fn of<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>() -> Self {
        Self {
            identity: TypeId::of::<(E, C)>(),
            #[cfg(test)]
            discard: super::server::discard_unpublished_server::<E, C>,
            class_factory: super::class_factory::dll_get_class_object::<E, C>,
            #[cfg(feature = "handles")]
            shutdown: super::server::shutdown::<E, C>,
        }
    }
}

static SERVER_TYPE: Mutex<Option<ServerType>> = Mutex::new(None);

/// Install before Addin::open can publish handles or subscription observations.
/// A different concrete type cannot replace a still-live active server.
pub(crate) fn register_server_type<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>()
-> XllResult<()> {
    let active = ACTIVE_SERVER.lock();
    let descriptor = ServerType::of::<E, C>();
    if active
        .as_ref()
        .is_some_and(|active| active.subscription_type != descriptor.identity)
    {
        return Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_MULTI,
        });
    }
    *SERVER_TYPE.lock() = Some(descriptor);
    Ok(())
}

/// Called with ACTIVE_SERVER locked, matching registration's lock order.
/// This prevents a previously selected observer from publishing a server after
/// another generation has replaced its constructor descriptor.
pub(super) fn is_registered<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>() -> bool {
    SERVER_TYPE
        .lock()
        .as_ref()
        .is_some_and(|selected| selected.identity == TypeId::of::<(E, C)>())
}

fn selected() -> Option<ServerType> {
    let selected = *SERVER_TYPE.lock();
    #[cfg(test)]
    let selected = selected.or_else(|| Some(ServerType::of::<(), crate::DynamicRtdSources<()>>()));
    selected
}

pub(crate) unsafe fn dll_get_class_object(
    class_id: *const c_void,
    interface_id: *const c_void,
    output: *mut *mut c_void,
) -> i32 {
    let Some(descriptor) = selected() else {
        if !output.is_null() {
            // SAFETY: COM supplies writable output storage.
            unsafe { *output = std::ptr::null_mut() };
        }
        return crate::win32::CLASS_E_CLASSNOTAVAILABLE;
    };
    // SAFETY: forward the validated COM ABI arguments unchanged; the typed
    // factory checks the active server's identity before casting its pointer.
    unsafe { (descriptor.class_factory)(class_id, interface_id, output) }
}

#[cfg(feature = "handles")]
pub(crate) fn shutdown(handles: &crate::formula_lifetime::FormulaLifetimeBackend) -> XllResult<()> {
    match selected() {
        Some(descriptor) => (descriptor.shutdown)(handles),
        None => {
            handles.terminate_all_topics();
            Ok(())
        }
    }
}

#[cfg(test)]
pub(super) fn discard_unpublished_server(pointer: usize, newly_created: bool) {
    if let Some(descriptor) = selected() {
        (descriptor.discard)(pointer, newly_created);
    }
}
