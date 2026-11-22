//! Structured worksheet and framework errors. Published types come from the
//! portable engine; callback decoding and service-slot conversion stay here.

pub(crate) use crate::value::Shape;
pub(crate) use xlfn_engine::error::internal as construct;
#[doc(inline)]
pub use xlfn_engine::error::{
    DomainErrorCode, ErrorCause, ExcelApiFailure, ExcelApiFunction, ExcelCallbackStatus,
    ExcelError, InputError, IntoXllError, InvalidLimit, UdfError, UdfResult, XllError, XllResult,
};

macro_rules! assert_excel_error_codes {
    ($(($variant:ident, $code:ident)),+ $(,)?) => {
        const _: () = {
            $(
                assert!(ExcelError::$variant.code() == xlfn_sys::$code);
                assert!(matches!(
                    ExcelError::from_code(xlfn_sys::$code),
                    Some(ExcelError::$variant)
                ));
            )+
        };
    };
}

assert_excel_error_codes!(
    (Null, XLERR_NULL),
    (DivisionByZero, XLERR_DIV0),
    (Value, XLERR_VALUE),
    (Reference, XLERR_REF),
    (Name, XLERR_NAME),
    (Number, XLERR_NUM),
    (NotAvailable, XLERR_NA),
    (GettingData, XLERR_GETTING_DATA),
);

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
