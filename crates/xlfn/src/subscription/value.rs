use crate::value::ExcelErrorValue;
use crate::{XllError, XllResult};
use triomphe::Arc;

#[derive(Clone, Debug, PartialEq)]
pub enum RtdValue {
    Number(f64),
    Boolean(bool),
    Integer(i32),
    String(String),
    Error(ExcelErrorValue),
    Empty,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StoredRtdValue {
    Number(f64),
    Boolean(bool),
    Integer(i32),
    String(Arc<String>),
    Error(ExcelErrorValue),
    Empty,
}

impl RtdValue {
    #[hotpath::measure(impl_type = "RtdValue")]
    pub(crate) fn validate(&self) -> XllResult<()> {
        match self {
            Self::Number(value) if !value.is_finite() => Err(XllError::Domain {
                code: crate::error::DomainErrorCode::InvalidInput,
            }),
            Self::String(value) => {
                crate::utf16::checked_utf16_len(
                    value,
                    "RTD value",
                    crate::utf16::EXCEL_STRING_LIMIT,
                )?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    #[hotpath::measure(impl_type = "RtdValue")]
    pub(crate) fn into_stored(self) -> XllResult<StoredRtdValue> {
        self.validate()?;
        Ok(match self {
            Self::Number(value) => StoredRtdValue::Number(value),
            Self::Boolean(value) => StoredRtdValue::Boolean(value),
            Self::Integer(value) => StoredRtdValue::Integer(value),
            Self::String(value) => StoredRtdValue::String(Arc::new(value)),
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
                Self::Error(crate::value::ExcelErrorValue(value))
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

pub trait IntoRtdValue {
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

impl IntoRtdValue for ExcelErrorValue {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::Error(self))
    }
}

impl IntoRtdValue for () {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::Empty)
    }
}
