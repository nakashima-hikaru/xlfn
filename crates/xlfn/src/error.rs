//! Structured worksheet and framework errors. Published types come from the
//! portable engine; callback decoding and service-slot conversion stay here.

pub(crate) use crate::value::Shape;
pub(crate) use xlfn_engine::error::internal as construct;
#[doc(inline)]
pub use xlfn_engine::error::{
    DomainErrorCode, ErrorCause, ExcelApiFailure, ExcelApiFunction, ExcelCallbackStatus,
    ExcelError, InputError, IntoXllError, InvalidLimit, UdfError, UdfResult, XllError, XllResult,
};

const _: () = {
    assert!(ExcelError::Null.code() == xlfn_sys::XLERR_NULL);
    assert!(matches!(
        ExcelError::from_code(xlfn_sys::XLERR_NULL),
        Some(ExcelError::Null)
    ));
    assert!(ExcelError::DivisionByZero.code() == xlfn_sys::XLERR_DIV0);
    assert!(matches!(
        ExcelError::from_code(xlfn_sys::XLERR_DIV0),
        Some(ExcelError::DivisionByZero)
    ));
    assert!(ExcelError::Value.code() == xlfn_sys::XLERR_VALUE);
    assert!(matches!(
        ExcelError::from_code(xlfn_sys::XLERR_VALUE),
        Some(ExcelError::Value)
    ));
    assert!(ExcelError::Reference.code() == xlfn_sys::XLERR_REF);
    assert!(matches!(
        ExcelError::from_code(xlfn_sys::XLERR_REF),
        Some(ExcelError::Reference)
    ));
    assert!(ExcelError::Name.code() == xlfn_sys::XLERR_NAME);
    assert!(matches!(
        ExcelError::from_code(xlfn_sys::XLERR_NAME),
        Some(ExcelError::Name)
    ));
    assert!(ExcelError::Number.code() == xlfn_sys::XLERR_NUM);
    assert!(matches!(
        ExcelError::from_code(xlfn_sys::XLERR_NUM),
        Some(ExcelError::Number)
    ));
    assert!(ExcelError::NotAvailable.code() == xlfn_sys::XLERR_NA);
    assert!(matches!(
        ExcelError::from_code(xlfn_sys::XLERR_NA),
        Some(ExcelError::NotAvailable)
    ));
    assert!(ExcelError::GettingData.code() == xlfn_sys::XLERR_GETTING_DATA);
    assert!(matches!(
        ExcelError::from_code(xlfn_sys::XLERR_GETTING_DATA),
        Some(ExcelError::GettingData)
    ));
};

#[cfg(any(feature = "handles", feature = "rtd"))]
pub(crate) fn map_service_slot_error(
    error: xlfn_kernel::service_slot::ServiceSlotError<XllError>,
) -> XllError {
    match error {
        xlfn_kernel::service_slot::ServiceSlotError::Closed => XllError::Closing,
        xlfn_kernel::service_slot::ServiceSlotError::Fault(fault) => match fault {
            xlfn_kernel::service_slot::ServiceFault::Error(error) => error,
            xlfn_kernel::service_slot::ServiceFault::Panicked => XllError::Panic,
        },
    }
}

pub(crate) const fn callback_status_from_raw(status: i32) -> ExcelCallbackStatus {
    match status {
        xlfn_sys::XLRET_SUCCESS => ExcelCallbackStatus::Success,
        xlfn_sys::XLRET_ABORT => ExcelCallbackStatus::Abort,
        xlfn_sys::XLRET_UNCALCED => ExcelCallbackStatus::Uncalced,
        other => ExcelCallbackStatus::Failed(other),
    }
}

pub(crate) const fn callback_status_is_terminal(status: ExcelCallbackStatus) -> bool {
    matches!(
        status,
        ExcelCallbackStatus::Abort | ExcelCallbackStatus::Uncalced
    )
}
