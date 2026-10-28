#![allow(
    clippy::multiple_unsafe_ops_per_block,
    reason = "Windows COM/VARIANT FFI operations are audited as compound ABI operations"
)]

#[cfg(test)]
use crate::XllError;
#[cfg(all(test, feature = "handles"))]
use crate::handle::FormulaHandleService;
#[cfg(test)]
use crate::subscription::RtdValue;
#[cfg(test)]
use crate::subscription::SubscriptionRuntime;
#[cfg(test)]
use crate::win32::{CO_E_SERVER_STOPPING, E_UNEXPECTED};
use crate::win32::{GUID, S_OK};
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
mod server_type;
mod subscription_backend;
mod update_event;
#[cfg(test)]
use crate::win32::{
    CoCreateGuid, DISPPARAMS, E_FAIL, E_INVALIDARG, VARIANT, VARIANT_FALSE, VARIANT_TRUE,
    VariantClear,
};
#[cfg(all(test, feature = "handles"))]
use crate::win32::{DISP_E_BADINDEX, E_NOTIMPL, E_POINTER, EXCEPINFO};
#[cfg(test)]
use crate::win32::{HKEY_CURRENT_USER, RegDeleteTreeW};
#[cfg(all(test, feature = "handles"))]
use automation::{
    DISPID_CONNECT_DATA, DISPID_DISCONNECT_DATA, DISPID_HEARTBEAT, DISPID_SERVER_START,
};
#[cfg(test)]
use automation::{
    DISPID_REFRESH_DATA, DISPID_SERVER_TERMINATE, IID_NULL, MAX_RTD_TOPIC_PARTS,
    checked_topic_part_count, checked_topic_part_length, topic_key_from_safearray,
    unwrap_dispatch_variant, write_bstr_variant, write_refresh_data,
};
#[cfg(test)]
use class_factory::{
    CLASS_FACTORY_VTABLE, ClassFactory, ClassFactoryVtable, IID_ICLASS_FACTORY, factory_lock_server,
};
#[cfg(test)]
use com_abi::IUnknown_Vtbl;
use com_abi::{IID_IUNKNOWN, com_boundary, guid_eq};
pub(crate) use event::drain_with_com_dispatch;
#[cfg(feature = "rtd")]
pub(super) use excel_rtd::observe_subscription;
pub(crate) use module_state::ComModuleLifetime;
#[cfg(test)]
use module_state::{ComObjectKind, ComObjectLease};
#[cfg(test)]
use registration::{
    CrossProcessRegistrationGuard, REGISTRATION_MAINTENANCE, RTD_PROG_ID_PREFIX,
    RTD_REGISTRATION_OWNER, RTD_REGISTRATION_SCHEMA, guid_braced, guid_compact,
    read_registry_string, scavenge_owned_registrations, set_registry_value, wide_nul,
};
pub(crate) use server::DeferredDisconnectLease;
#[cfg(all(test, feature = "handles"))]
use server::ensure_server;
#[cfg(feature = "rtd")]
pub(super) use server::shutdown_subscriptions;
use server::{
    ACTIVE_SERVER, ActiveServer, IID_IRTD_UPDATE_EVENT, RtdServer, connect_data, disconnect_data,
    heartbeat, refresh_data, server_add_ref, server_query_interface, server_release, server_start,
    server_terminate,
};
#[cfg(all(test, feature = "handles"))]
use server::{
    FAIL_DEFERRED_TERMINATION_SPAWN, PANIC_DEFERRED_TERMINATION_CLEANUP,
    synchronize_callback_notification,
};
#[cfg(test)]
use server::{
    IID_IDISPATCH, IID_IRTD_SERVER, PANIC_IN_REFRESH_DATA, SERVER_NOT_STARTED, SERVER_START_FAILED,
    SERVER_STARTED, SERVER_STARTING, ServerStartReservation, ensure_server_without_handles,
};
#[cfg(test)]
use server_gate::{
    ServerCloseError, ServerNotificationOperation, ServerOperation, ServerOperationBarrier,
    ServerPhase, ServerTermination, ServerTerminationRequest, TerminationWorker,
    TerminationWorkerStatus,
};
#[cfg(feature = "handles")]
pub(super) use server_type::observe;
#[cfg(feature = "handles")]
pub(super) use server_type::shutdown;
pub(super) use server_type::{dll_get_class_object, register_server_type};
pub(crate) use update_event::ComApartmentGuard;
pub(crate) use update_event::RtdNotifier;
use update_event::retry_git_revocation_debt;
#[cfg(test)]
use update_event::{RetainedUpdateCallback, install_callback, retry_git_revocation_debt_with};

pub(super) fn module_lifetime() -> &'static ComModuleLifetime {
    crate::module_runtime::global().com_module_lifetime()
}

#[cfg(any(test, feature = "refinement"))]
pub(super) fn connect_trace<F>(
    trace: crate::shutdown_trace::ShutdownTraceHandle,
    initialize: F,
) -> crate::XllResult<()>
where
    F: FnOnce(crate::excel_rtd_protocol::RtdTraceSnapshot) -> crate::XllResult<()>,
{
    module_lifetime().connect_trace(trace, initialize)
}

#[cfg(any(test, feature = "refinement"))]
pub(super) fn trace_snapshot() -> crate::excel_rtd_protocol::RtdTraceSnapshot {
    module_lifetime().trace_snapshot()
}

pub(super) fn physical_unload_ready() -> bool {
    module_lifetime().can_unload_now()
}

pub(super) fn dll_can_unload_now() -> i32 {
    if crate::excel_rtd_protocol::logical_quiescence_certified()
        && module_lifetime().can_unload_now()
    {
        S_OK
    } else {
        1 // S_FALSE
    }
}

pub(super) fn wait_for_module_quiescence(
    module_residency_held: bool,
) -> Result<(), crate::excel_rtd_protocol::RtdQuiescenceError> {
    module_lifetime()
        .wait_for_logical_quiescence(retry_git_revocation_debt, module_residency_held)
        .map_err(|error| crate::excel_rtd_protocol::RtdQuiescenceError {
            outstanding_git_cookies: error.state.outstanding_git_cookies,
            revocation_debt: error.state.revocation_debt,
            missing_module_residency: !module_residency_held
                && (error.state.live_factories != 0
                    || error.state.live_servers != 0
                    || error.state.server_locks != 0),
        })
}

#[cfg(test)]
mod tests;
#[cfg(test)]
use server_type::discard_unpublished_server;
