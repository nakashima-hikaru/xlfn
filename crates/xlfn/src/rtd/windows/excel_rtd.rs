use super::registration::TemporaryRegistration;
#[cfg(feature = "handles")]
use super::server::ensure_server;
use super::server::{
    RtdServer, SERVER_STARTED, discard_unpublished_server, ensure_server_without_handles,
};
#[cfg(feature = "handles")]
use crate::XllError;
use crate::XllResult;
#[cfg(feature = "handles")]
use crate::handle::{FormulaLifetimeBackend, FormulaLifetimeGeneration};
use crate::host_api::ExcelHost;
#[cfg(feature = "handles")]
use crate::ingress::ExportIngress;
use crate::subscription::{RtdValue, SubscriptionRuntime};
use crate::value::ExcelValue;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use xlfn_sys::{XLF_RTD, XLOPER12, XLOPER12Value, XLTYPE_STR};

#[cfg(feature = "handles")]
pub(crate) fn observe<H: FormulaLifetimeBackend + 'static>(
    handles: Arc<H>,
    ingress: &'static ExportIngress,
    rtd_key: &str,
    token: &str,
    host: ExcelHost<'_>,
) -> XllResult<()> {
    let _rtd_operation = crate::excel_rtd::begin_operation(handles.as_ref(), ingress)?;
    let ensured = ensure_server(Some(&handles), None)?;
    let active = &ensured.active;
    let server = active.pointer as *mut RtdServer;

    // SAFETY: ACTIVE_SERVER owns a live server reference and `ensured` holds a
    // separate temporary reference throughout `observe`.
    let registration = if unsafe { (*server).start_state.load(Ordering::Acquire) } == SERVER_STARTED
    {
        None
    } else {
        let module_path = match module_path(host) {
            Ok(path) => path,
            Err(error) => {
                discard_unpublished_server(active.pointer, ensured.newly_created);
                return Err(error);
            }
        };

        match TemporaryRegistration::new(active, &module_path) {
            Ok(registration) => Some(registration),
            Err(error) => {
                discard_unpublished_server(active.pointer, ensured.newly_created);
                return Err(error);
            }
        }
    };

    let mut prog_id = match CountedString::new(&active.prog_id) {
        Ok(value) => value,
        Err(error) => {
            discard_unpublished_server(active.pointer, ensured.newly_created);
            return Err(error);
        }
    };

    let mut topic = match CountedString::new(&format!("handle:{rtd_key}")) {
        Ok(value) => value,
        Err(error) => {
            discard_unpublished_server(active.pointer, ensured.newly_created);
            return Err(error);
        }
    };

    let lifetime_generation = FormulaLifetimeGeneration::new(active.generation.get())
        .expect("an active Excel RTD server has a non-zero lifetime generation");
    if let Err(error) = handles.claim_lifetime(rtd_key, lifetime_generation) {
        discard_unpublished_server(active.pointer, ensured.newly_created);
        return Err(error);
    }

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

    Ok(())
}

pub(crate) fn observe_subscription(
    subscriptions: &Arc<SubscriptionRuntime>,
    key: &crate::subscription::SubscriptionKey,
    host: ExcelHost<'_>,
) -> XllResult<RtdValue> {
    let _rtd_operation = subscriptions.enter_external_operation()?;
    let ensured = ensure_server_without_handles(Some(subscriptions))?;
    let active = &ensured.active;
    let server = active.pointer as *mut RtdServer;

    // SAFETY: ACTIVE_SERVER owns a live server reference and `ensured` holds a
    // separate temporary reference throughout this function.
    let registration = if unsafe { (*server).start_state.load(Ordering::Acquire) } == SERVER_STARTED
    {
        None
    } else {
        let module_path = match module_path(host) {
            Ok(path) => path,
            Err(error) => {
                discard_unpublished_server(active.pointer, ensured.newly_created);
                return Err(error);
            }
        };

        match TemporaryRegistration::new(active, &module_path) {
            Ok(registration) => Some(registration),
            Err(error) => {
                discard_unpublished_server(active.pointer, ensured.newly_created);
                return Err(error);
            }
        }
    };

    let mut prog_id = match CountedString::new(&active.prog_id) {
        Ok(value) => value,
        Err(error) => {
            discard_unpublished_server(active.pointer, ensured.newly_created);
            return Err(error);
        }
    };

    let key_transport = key.to_transport();
    let mut topic = match CountedString::new(&key_transport) {
        Ok(value) => value,
        Err(error) => {
            discard_unpublished_server(active.pointer, ensured.newly_created);
            return Err(error);
        }
    };

    let id = subscriptions.resolve_transport_key(*key)?;
    if let Some(subscription_server) = &ensured.subscription_server
        && let Err(error) = subscription_server.claim(id)
    {
        discard_unpublished_server(active.pointer, ensured.newly_created);
        return Err(error);
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

    RtdValue::try_from(value)
}

fn module_path(host: ExcelHost<'_>) -> XllResult<String> {
    host.module_path()
}

struct CountedString {
    units: Box<[u16]>,
    oper: XLOPER12,
}

impl CountedString {
    fn new(value: &str) -> XllResult<Self> {
        let units =
            crate::utf16::encode_counted(value, "RTD topic", crate::utf16::EXCEL_STRING_LIMIT)?;
        let mut units = units.into_boxed_slice();
        let oper = XLOPER12 {
            value: XLOPER12Value {
                string: units.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };

        Ok(Self { units, oper })
    }

    fn pointer(&mut self) -> NonNull<XLOPER12> {
        let _keep_alive = &self.units;
        NonNull::from_mut(&mut self.oper)
    }
}
