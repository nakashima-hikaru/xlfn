use super::registration::TemporaryRegistration;
#[cfg(feature = "handles")]
use super::server::ensure_server;
use super::server::{SERVER_STARTED, ServerObservation, ensure_server_without_handles};
use super::subscription_backend::BackendSubscriptions;
#[cfg(feature = "handles")]
use crate::XllError;
use crate::XllResult;
use crate::excel_rtd::counted_string::CountedString;
#[cfg(feature = "handles")]
use crate::formula_lifetime::{FormulaLifetimeBackend, FormulaLifetimeGeneration};
use crate::host_api::ExcelHost;
#[cfg(feature = "handles")]
use crate::ingress::ExportIngress;
use crate::subscription::{RtdValue, SubscriptionRuntime};
use crate::value::ExcelValue;
use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use xlfn_sys::{XLF_RTD, XLOPER12};

#[cfg(feature = "handles")]
pub(crate) fn observe<E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    handles: &FormulaLifetimeBackend,
    ingress: &'static ExportIngress,
    rtd_key: &str,
    token: &str,
    host: ExcelHost<'_>,
) -> XllResult<()> {
    let _rtd_operation = crate::excel_rtd::begin_operation(handles, ingress)?;
    let mut observation = ServerObservation::begin(ensure_server::<E, C>(Some(handles), None)?)?;
    let ensured = observation.server();
    let active = &ensured.active;
    let server = ensured.pointer();

    // SAFETY: ACTIVE_SERVER owns a live server reference and `ensured` holds a
    // separate temporary reference throughout `observe`.
    let registration = if unsafe { (*server).start_state.load(Ordering::Acquire) } == SERVER_STARTED
    {
        None
    } else {
        let module_path = module_path(host)?;
        Some(TemporaryRegistration::new(active, &module_path)?)
    };

    let mut prog_id = CountedString::new(&active.prog_id)?;
    let mut topic = CountedString::new(&format!("handle:{rtd_key}"))?;

    let lifetime_generation = FormulaLifetimeGeneration::new(active.generation.get())
        .expect("an active Excel RTD server has a non-zero lifetime generation");
    handles.claim_lifetime(rtd_key, lifetime_generation)?;

    let mut server_name = XLOPER12::missing();
    let arguments = [
        prog_id.pointer(),
        NonNull::from_mut(&mut server_name),
        topic.pointer(),
    ];

    let returned = host.invoke(
        XLF_RTD,
        crate::error::ExcelApiFunction::Rtd,
        &arguments,
        |result| <String as crate::value::FromExcel>::from_excel(result.borrow()?, "RTD handle"),
    )?;

    drop(registration);

    if returned != token {
        return Err(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::RTD_HANDLE,
        });
    }

    observation.commit();
    Ok(())
}

pub(crate) fn observe_subscription<E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    subscriptions: &SubscriptionRuntime<E, C>,
    key: &crate::subscription::SubscriptionKey,
    host: ExcelHost<'_>,
) -> XllResult<RtdValue> {
    let _rtd_operation = subscriptions.enter_external_operation()?;
    // Reject a key from another runtime before allocating or attaching a server.
    let _ = subscriptions.resolve_transport_key(*key)?;
    let mut observation = ServerObservation::begin(ensure_server_without_handles(Some(
        BackendSubscriptions::new(subscriptions),
    ))?)?;
    let ensured = observation.server();
    let active = &ensured.active;
    let server = ensured.pointer();

    // SAFETY: ACTIVE_SERVER owns a live server reference and `ensured` holds a
    // separate temporary reference throughout this function.
    let registration = if unsafe { (*server).start_state.load(Ordering::Acquire) } == SERVER_STARTED
    {
        None
    } else {
        let module_path = module_path(host)?;
        Some(TemporaryRegistration::new(active, &module_path)?)
    };

    let mut prog_id = CountedString::new(&active.prog_id)?;
    let key_transport = key.to_transport();
    let mut topic = CountedString::new(key_transport.as_str())?;
    if let Some(subscription_server) = &ensured.subscription_server {
        subscription_server.claim(*key)?;
    }

    let mut server_name = XLOPER12::missing();
    let arguments = [
        prog_id.pointer(),
        NonNull::from_mut(&mut server_name),
        topic.pointer(),
    ];

    let value = host.invoke(
        XLF_RTD,
        crate::error::ExcelApiFunction::Rtd,
        &arguments,
        |result| <ExcelValue as crate::value::FromExcel>::from_excel(result.borrow()?, "RTD value"),
    )?;

    drop(registration);

    let value = RtdValue::try_from(value)?;
    observation.commit();
    Ok(value)
}

fn module_path(host: ExcelHost<'_>) -> XllResult<String> {
    host.module_path()
}
