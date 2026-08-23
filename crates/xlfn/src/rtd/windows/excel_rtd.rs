use super::registration::TemporaryRegistration;
use super::server::{RtdServer, SERVER_STARTED, discard_unpublished_server, ensure_server};
use crate::error::{ExcelApiFailure, ExcelApiFunction};
use crate::handle::FormulaHandleService;
use crate::host_callback::HostCallbackSession;
use crate::ingress::ExportIngress;
use crate::subscription::{RtdValue, SubscriptionRuntime};
use crate::value::{ExcelValue, FromExcel};
use crate::{ExcelCallbackStatus, XllError, XllResult};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use xlfn_sys::{XL_GET_NAME, XLF_RTD, XLOPER12, XLOPER12Value, XLTYPE_STR};

pub(crate) fn observe(
    handles: &Arc<FormulaHandleService>,
    ingress: &'static ExportIngress,
    rtd_key: &str,
    token: &str,
    callbacks: &HostCallbackSession,
) -> XllResult<()> {
    let _rtd_operation = crate::rtd::begin_operation(handles, ingress)?;
    let ensured = ensure_server(Some(handles), None)?;
    let active = &ensured.active;
    let server = active.pointer as *mut RtdServer;

    // SAFETY: ACTIVE_SERVER owns a live server reference and `ensured` holds a
    // separate temporary reference throughout `observe`.
    let registration = if unsafe { (*server).start_state.load(Ordering::Acquire) } == SERVER_STARTED
    {
        None
    } else {
        let module_path = match module_path(callbacks) {
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

    if let Err(error) = handles.claim_server(rtd_key, active.generation) {
        discard_unpublished_server(active.pointer, ensured.newly_created);
        return Err(error);
    }

    let mut server_name = XLOPER12::missing();
    let arguments = [
        prog_id.pointer(),
        NonNull::from_mut(&mut server_name),
        topic.pointer(),
    ];

    // SAFETY: every pointer in `arguments` refers to a live XLOPER12 that
    // remains valid and stationary for the duration of the Excel callback.
    let (status, mut result) = unsafe {
        callbacks
            .call(XLF_RTD, &arguments)
            .map_err(|suppressed| XllError::ExcelApi {
                function: ExcelApiFunction::Rtd,
                failure: ExcelApiFailure::Suppressed(suppressed.status),
            })?
    };

    drop(registration);

    if status != ExcelCallbackStatus::Success {
        return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
            function: ExcelApiFunction::Rtd,
            failure: ExcelApiFailure::Status(status),
        }));
    }

    let returned = <String as FromExcel>::from_excel(result.borrow()?, "RTD handle")?;
    result.try_release()?;

    if returned != token {
        return Err(XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::RTD_HANDLE,
        });
    }

    Ok(())
}

pub(crate) fn observe_subscription(
    subscriptions: &Arc<SubscriptionRuntime>,
    key: &crate::subscription::SubscriptionKey,
    callbacks: &HostCallbackSession,
) -> XllResult<RtdValue> {
    let _rtd_operation = subscriptions.enter_external_operation()?;
    let ensured = ensure_server(None, Some(subscriptions))?;
    let active = &ensured.active;
    let server = active.pointer as *mut RtdServer;

    // SAFETY: ACTIVE_SERVER owns a live server reference and `ensured` holds a
    // separate temporary reference throughout this function.
    let registration = if unsafe { (*server).start_state.load(Ordering::Acquire) } == SERVER_STARTED
    {
        None
    } else {
        let module_path = match module_path(callbacks) {
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

    if let Some(subscription_server) = &ensured.subscription_server
        && let Err(error) = subscription_server.claim(key)
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

    // SAFETY: every pointer in `arguments` refers to a live XLOPER12 that
    // remains valid and stationary for the duration of the Excel callback.
    let (status, mut result) = unsafe {
        callbacks
            .call(XLF_RTD, &arguments)
            .map_err(|suppressed| XllError::ExcelApi {
                function: ExcelApiFunction::Rtd,
                failure: ExcelApiFailure::Suppressed(suppressed.status),
            })?
    };

    drop(registration);

    if status != ExcelCallbackStatus::Success {
        return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
            function: ExcelApiFunction::Rtd,
            failure: ExcelApiFailure::Status(status),
        }));
    }

    let value = <ExcelValue as FromExcel>::from_excel(result.borrow()?, "RTD value")?;
    result.try_release()?;

    RtdValue::try_from(value)
}

fn module_path(callbacks: &HostCallbackSession) -> XllResult<String> {
    // SAFETY: xlGetName takes no arguments. ExcelCallbackValue assumes ownership
    // of the callback result and exposes it through its managed result wrapper.
    let (status, mut result) = unsafe {
        callbacks
            .call(XL_GET_NAME, &[])
            .map_err(|suppressed| XllError::ExcelApi {
                function: ExcelApiFunction::GetName,
                failure: ExcelApiFailure::Suppressed(suppressed.status),
            })?
    };

    if status != ExcelCallbackStatus::Success {
        return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
            function: ExcelApiFunction::GetName,
            failure: ExcelApiFailure::Status(status),
        }));
    }

    let path = <String as FromExcel>::from_excel(result.borrow()?, "module")?;
    result.try_release()?;

    Ok(path)
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
