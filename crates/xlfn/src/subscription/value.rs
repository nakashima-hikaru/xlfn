use crate::ExcelError;
use crate::error::InputError;
use crate::value::FromExcel;
use crate::value::raw::{XlValueRef, XlValueType};
use crate::{XllError, XllResult};

/// An owned scalar candidate for RTD publication.
///
/// Public variants allow application code to construct and inspect values.
/// Publication through [`super::source::RtdSink::publish`] or
/// `RtdSender::try_send` validates finite numbers and Excel's UTF-16
/// string limit before the value enters runtime storage. Constructing this
/// enum alone does not establish those constraints.
#[derive(Clone, Debug, PartialEq)]
pub enum RtdValue {
    /// A number that must be finite when published.
    Number(f64),
    /// A Boolean scalar.
    Boolean(bool),
    /// A signed integer scalar.
    Integer(i32),
    /// An owned string subject to Excel's UTF-16 limit at publication.
    String(String),
    /// An intentional Excel error value.
    Error(ExcelError),
    /// No current RTD value.
    Empty,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StoredRtdValue {
    Number(f64),
    Boolean(bool),
    Integer(i32),
    String(Box<str>),
    Error(ExcelError),
    Empty,
}

impl RtdValue {
    /// Decodes the supported scalar payloads while the callback result is live.
    /// A Multi is rejected without reading its payload. Defer that semantic
    /// error until after the host successfully releases the callback result.
    pub(crate) fn decode_excel_observation(value: XlValueRef<'_>) -> XllResult<Option<Self>> {
        const ARGUMENT: &str = "RTD value";
        match value.value_type() {
            XlValueType::Number | XlValueType::Integer => {
                f64::from_excel(value, ARGUMENT).map(Self::Number)
            }
            XlValueType::Boolean => bool::from_excel(value, ARGUMENT).map(Self::Boolean),
            XlValueType::String => String::from_excel(value, ARGUMENT).map(Self::String),
            XlValueType::Error => ExcelError::from_excel(value, ARGUMENT).map(Self::Error),
            XlValueType::Missing | XlValueType::Nil => Ok(Self::Empty),
            XlValueType::Multi => return Ok(None),
            _ => Err(value.wrong_type(ARGUMENT, "worksheet value")),
        }
        .map(Some)
    }

    pub(crate) fn scalar_required_error() -> XllError {
        XllError::validation(
            "RTD value",
            InputError::Malformed("RTD values must be scalar"),
        )
    }

    pub(crate) fn validate(&self) -> XllResult<()> {
        match self {
            Self::Number(value) if !value.is_finite() => Err(XllError::Domain {
                code: crate::error::DomainErrorCode::InvalidInput,
            }),
            Self::String(value) => crate::utf16::validate_utf16_limit(
                value,
                "RTD value",
                crate::utf16::EXCEL_STRING_LIMIT,
            ),
            _ => Ok(()),
        }
    }

    pub(crate) fn into_stored(self) -> XllResult<StoredRtdValue> {
        self.validate()?;
        Ok(match self {
            Self::Number(value) => StoredRtdValue::Number(value),
            Self::Boolean(value) => StoredRtdValue::Boolean(value),
            Self::Integer(value) => StoredRtdValue::Integer(value),
            Self::String(value) => StoredRtdValue::String(value.into_boxed_str()),
            Self::Error(value) => StoredRtdValue::Error(value),
            Self::Empty => StoredRtdValue::Empty,
        })
    }
}

impl TryFrom<crate::value::ExcelValue> for RtdValue {
    type Error = XllError;

    fn try_from(value: crate::value::ExcelValue) -> XllResult<Self> {
        let value = match value {
            crate::value::ExcelValue::Scalar(crate::value::ExcelCellValue::Number(value)) => {
                Self::Number(value)
            }
            crate::value::ExcelValue::Scalar(crate::value::ExcelCellValue::Boolean(value)) => {
                Self::Boolean(value)
            }
            crate::value::ExcelValue::Scalar(crate::value::ExcelCellValue::String(value)) => {
                Self::String(value)
            }
            crate::value::ExcelValue::Scalar(crate::value::ExcelCellValue::Error(value)) => {
                Self::Error(value)
            }
            crate::value::ExcelValue::Missing
            | crate::value::ExcelValue::Scalar(crate::value::ExcelCellValue::Blank) => Self::Empty,
            crate::value::ExcelValue::Array(_) => return Err(Self::scalar_required_error()),
        };
        value.validate()?;
        Ok(value)
    }
}

/// Converts an application value into an owned RTD scalar candidate.
///
/// `Ok` reports successful application conversion, not admission or delivery.
/// Implementations may reject application-specific invalid inputs. The
/// framework validates every resulting [`RtdValue`] at publication, including
/// values returned by custom implementations, so converters do not need to
/// duplicate the finite-number or UTF-16 string checks.
///
/// Conversion runs on the publishing thread before runtime storage is locked.
pub trait IntoRtdValue {
    /// Converts an application value; publication validates the candidate.
    fn into_rtd_value(self) -> XllResult<RtdValue>;
}

impl IntoRtdValue for RtdValue {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(self)
    }
}

impl IntoRtdValue for f64 {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        if self.is_finite() {
            Ok(RtdValue::Number(self))
        } else {
            Err(XllError::Domain {
                code: crate::error::DomainErrorCode::InvalidInput,
            })
        }
    }
}

impl IntoRtdValue for bool {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::Boolean(self))
    }
}

impl IntoRtdValue for i32 {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::Integer(self))
    }
}

impl IntoRtdValue for i64 {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        const EXACT_LIMIT: i64 = 1_i64 << 53;
        if (-EXACT_LIMIT..=EXACT_LIMIT).contains(&self) {
            Ok(RtdValue::Number(self as f64))
        } else {
            Err(XllError::Domain {
                code: crate::error::DomainErrorCode::Overflow,
            })
        }
    }
}

impl IntoRtdValue for crate::value::ExcelSerialDate {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        self.serial().into_rtd_value()
    }
}

impl IntoRtdValue for String {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::String(self))
    }
}

impl IntoRtdValue for &str {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::String(self.to_owned()))
    }
}

impl IntoRtdValue for ExcelError {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::Error(self))
    }
}

impl IntoRtdValue for () {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::Empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{ExcelCellValue, ExcelValue};
    use xlfn_sys::{XLOPER12, XLOPER12Array, XLOPER12Value, XLTYPE_FLOW, XLTYPE_MULTI, XLTYPE_STR};

    fn decode(raw: &XLOPER12) -> XllResult<Option<RtdValue>> {
        RtdValue::decode_excel_observation(XlValueRef::from_array_cell(raw)?)
    }

    #[test]
    fn miri_observation_decodes_supported_excel_scalars() {
        let mut text = [3, 0x65e5, 0xd83d, 0xdca1];
        let cases = [
            (XLOPER12::number(-2.5), RtdValue::Number(-2.5)),
            (XLOPER12::integer(-7), RtdValue::Number(-7.0)),
            (XLOPER12::boolean(true), RtdValue::Boolean(true)),
            (
                XLOPER12 {
                    value: XLOPER12Value {
                        string: text.as_mut_ptr(),
                    },
                    xltype: XLTYPE_STR,
                },
                RtdValue::String("日💡".to_owned()),
            ),
            (
                XLOPER12::error(ExcelError::NotAvailable.code()),
                RtdValue::Error(ExcelError::NotAvailable),
            ),
            (XLOPER12::nil(), RtdValue::Empty),
            (XLOPER12::missing(), RtdValue::Empty),
        ];
        for (raw, expected) in cases {
            assert_eq!(decode(&raw).unwrap(), Some(expected));
        }
    }

    #[test]
    fn miri_observation_rejects_invalid_scalar_payloads() {
        let mut surrogate = [1, 0xd800];
        let mut oversized = vec![0_u16; crate::utf16::EXCEL_STRING_LIMIT + 2];
        oversized[0] = crate::utf16::EXCEL_STRING_LIMIT as u16 + 1;
        let cases = [
            (XLOPER12::number(f64::INFINITY), InputError::NonFinite),
            (
                XLOPER12::error(-1),
                InputError::Malformed("unknown error code"),
            ),
            (
                XLOPER12 {
                    xltype: XLTYPE_FLOW,
                    ..XLOPER12::nil()
                },
                InputError::WrongType {
                    expected: "worksheet value",
                    actual: XlValueType::Flow,
                },
            ),
            (
                XLOPER12 {
                    value: XLOPER12Value {
                        string: surrogate.as_mut_ptr(),
                    },
                    xltype: XLTYPE_STR,
                },
                InputError::InvalidUtf16,
            ),
            (
                XLOPER12 {
                    value: XLOPER12Value {
                        string: oversized.as_mut_ptr(),
                    },
                    xltype: XLTYPE_STR,
                },
                InputError::TooLarge {
                    limit: crate::utf16::EXCEL_STRING_LIMIT,
                    actual: crate::utf16::EXCEL_STRING_LIMIT + 1,
                },
            ),
        ];
        for (raw, expected) in cases {
            assert!(matches!(
                decode(&raw),
                Err(XllError::Input { argument: "RTD value", reason }) if reason == expected
            ));
        }
    }

    #[test]
    fn miri_observation_classifies_multi_without_reading_an_invalid_cell() {
        let mut invalid_cell = XLOPER12::number(f64::NAN);
        let raw = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    rows: 1,
                    columns: 1,
                    values: &mut invalid_cell,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let raw = XlValueRef::from_array_cell(&raw).unwrap();
        assert!(matches!(
            ExcelValue::from_excel(raw, "RTD value"),
            Err(XllError::Input {
                reason: InputError::NonFinite,
                ..
            })
        ));
        assert_eq!(RtdValue::decode_excel_observation(raw).unwrap(), None);
    }

    #[test]
    fn miri_observation_preserves_callback_and_cleanup_error_priority() {
        use crate::error::{ExcelApiFailure, ExcelApiFunction};
        use crate::host_api::ExcelHost;
        use crate::host_callback::HostCallbackSession;
        use crate::registration::host::test_support::{CallbackScript, Reply};
        use crate::return_abi::ExcelCallbackStatus;
        use xlfn_sys::{XLF_RTD, XLRET_ABORT, XLRET_FAILED, XLRET_SUCCESS};

        let mut cell = XLOPER12::number(f64::NAN);
        let multi = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    rows: 1,
                    columns: 1,
                    values: &mut cell,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let observe = |value, status, free| {
            let script = CallbackScript::install([Reply::success(XLF_RTD, value)
                .status(status)
                .release_status(free)]);
            let session = HostCallbackSession::new();
            let host = ExcelHost::new(&session);
            let value = host.invoke(XLF_RTD, ExcelApiFunction::Rtd, &[], |result| {
                RtdValue::decode_excel_observation(result.borrow()?)
            });
            script.assert_calls(&[XLF_RTD]);
            value.and_then(|value| value.ok_or_else(RtdValue::scalar_required_error))
        };

        assert_eq!(
            observe(XLOPER12::integer(-7), XLRET_SUCCESS, XLRET_SUCCESS).unwrap(),
            RtdValue::Number(-7.0)
        );
        assert!(matches!(
            observe(XLOPER12::number(f64::NAN), XLRET_SUCCESS, XLRET_FAILED),
            Err(XllError::Input {
                reason: InputError::NonFinite,
                ..
            })
        ));
        assert!(matches!(
            observe(multi, XLRET_SUCCESS, XLRET_SUCCESS),
            Err(XllError::Application { message, .. }) if message == "invalid RTD value"
        ));
        assert!(matches!(
            observe(multi, XLRET_SUCCESS, XLRET_FAILED),
            Err(XllError::ExcelApi {
                function: ExcelApiFunction::Free,
                failure: ExcelApiFailure::Status(ExcelCallbackStatus::Failed(XLRET_FAILED)),
            })
        ));
        for status in [XLRET_ABORT, XLRET_FAILED] {
            assert!(matches!(
                observe(multi, status, XLRET_SUCCESS),
                Err(XllError::ExcelApi {
                    function: ExcelApiFunction::Rtd,
                    failure: ExcelApiFailure::Status(actual),
                }) if actual == ExcelCallbackStatus::from_raw(status)
            ));
        }
    }

    #[test]
    fn excel_error_is_preserved_in_direct_and_dynamic_rtd_values() {
        let error = ExcelError::NotAvailable;
        assert_eq!(error.into_rtd_value().unwrap(), RtdValue::Error(error));
        assert_eq!(
            RtdValue::try_from(ExcelValue::Scalar(ExcelCellValue::Error(error))).unwrap(),
            RtdValue::Error(error)
        );
        assert_eq!(
            RtdValue::Error(error).into_stored().unwrap(),
            StoredRtdValue::Error(error)
        );
    }
}
