use crate::ExcelError;
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
            crate::value::ExcelValue::Array(_) => {
                return Err(XllError::input(
                    "RTD value",
                    crate::error::InputError::Malformed("RTD values must be scalar"),
                ));
            }
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
