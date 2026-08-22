#[cfg(test)]
use crate::XllError;
#[cfg(test)]
use crate::XllResult;
use crate::handle::FormulaHandleService;
#[cfg(test)]
use crate::subscription::RtdValue;
#[cfg(test)]
use crate::subscription::SubscriptionRuntime;
#[cfg(test)]
use crate::win32::{CO_E_SERVER_STOPPING, E_UNEXPECTED};
use crate::win32::{GUID, S_OK};
#[cfg(test)]
use parking_lot::Mutex;
#[cfg(test)]
use std::ffi::c_void;
#[cfg(test)]
use std::num::NonZeroU32;
#[cfg(test)]
use std::panic::{AssertUnwindSafe, catch_unwind};
#[cfg(test)]
use std::ptr::NonNull;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::Ordering;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32};

mod automation;
mod class_factory;
mod com_abi;
mod event;
mod excel_rtd;
mod global_interface_table;
mod module_state;
mod registration;
mod server;
mod server_gate;
mod update_event;
#[cfg(test)]
use crate::win32::{
    CoCreateGuid, DISP_E_BADINDEX, DISPPARAMS, E_FAIL, E_INVALIDARG, E_NOINTERFACE, E_NOTIMPL,
    E_POINTER, EXCEPINFO, VARIANT, VARIANT_FALSE, VARIANT_TRUE, VariantClear,
};
#[cfg(test)]
use crate::win32::{HKEY_CURRENT_USER, RegDeleteTreeW};
#[cfg(test)]
use automation::{
    DISPID_CONNECT_DATA, DISPID_DISCONNECT_DATA, DISPID_HEARTBEAT, DISPID_REFRESH_DATA,
    DISPID_SERVER_START, DISPID_SERVER_TERMINATE, IID_NULL, MAX_RTD_TOPIC_PARTS,
    checked_topic_part_count, checked_topic_part_length, topic_key_from_safearray,
    unwrap_dispatch_variant, write_bstr_variant, write_refresh_data,
};
pub(super) use class_factory::dll_get_class_object;
#[cfg(test)]
use class_factory::{
    CLASS_FACTORY_VTABLE, ClassFactory, ClassFactoryVtable, IID_ICLASS_FACTORY,
    factory_lock_server, factory_release,
};
#[cfg(test)]
use com_abi::IUnknown_Vtbl;
use com_abi::{IID_IUNKNOWN, com_boundary, guid_eq};
pub(super) use excel_rtd::{observe, observe_subscription};
pub(crate) use module_state::ComModuleLifetime;
#[cfg(test)]
use module_state::{ComObjectKind, ComObjectLease};
#[cfg(test)]
use registration::{
    CrossProcessRegistrationGuard, REGISTRATION_MAINTENANCE, RTD_PROG_ID_PREFIX,
    RTD_REGISTRATION_OWNER, RTD_REGISTRATION_SCHEMA, guid_braced, guid_compact,
    read_registry_string, scavenge_owned_registrations, set_registry_value, wide_nul,
};
use server::{
    ACTIVE_SERVER, ActiveServer, IID_IRTD_UPDATE_EVENT, RtdServer, connect_data, disconnect_data,
    heartbeat, refresh_data, server_add_ref, server_query_interface, server_release, server_start,
    server_terminate,
};
#[cfg(test)]
use server::{
    FAIL_DEFERRED_TERMINATION_SPAWN, IID_IDISPATCH, IID_IRTD_SERVER,
    PANIC_DEFERRED_TERMINATION_CLEANUP, PANIC_IN_REFRESH_DATA, SERVER_NOT_STARTED,
    SERVER_START_FAILED, SERVER_STARTED, SERVER_STARTING, ServerStartReservation,
    discard_unpublished_server, ensure_server, synchronize_callback_notification,
};
pub(super) use server::{shutdown, shutdown_subscriptions};
#[cfg(test)]
use server_gate::{
    ServerCloseError, ServerNotificationOperation, ServerOperation, ServerOperationBarrier,
    ServerPhase, ServerTermination, ServerTerminationRequest, TerminationWorker,
    TerminationWorkerStatus,
};
pub(crate) use update_event::RtdNotifier;
use update_event::retry_git_revocation_debt;
#[cfg(test)]
use update_event::{RetainedUpdateCallback, install_callback, retry_git_revocation_debt_with};

pub(super) fn module_lifetime() -> &'static ComModuleLifetime {
    crate::module_runtime::global().com_module_lifetime()
}

#[cfg(any(test, feature = "unstable"))]
pub(super) fn set_ghost(ghost: crate::shutdown_refinement::GhostHandle) {
    module_lifetime().set_ghost(ghost);
}

pub(super) fn dll_can_unload_now() -> i32 {
    if crate::rtd::logical_quiescence_certified() && module_lifetime().can_unload_now() {
        S_OK
    } else {
        1 // S_FALSE
    }
}

pub(super) fn wait_for_module_quiescence() -> Result<(), crate::rtd::RtdQuiescenceError> {
    module_lifetime()
        .wait_for_quiescence(retry_git_revocation_debt)
        .map_err(|error| crate::rtd::RtdQuiescenceError {
            outstanding_git_cookies: error.state.outstanding_git_cookies,
            revocation_debt: error.state.revocation_debt,
        })
}

#[cfg(test)]
mod tests;
