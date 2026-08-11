use super::*;

#[derive(Clone, Debug, PartialEq)]
pub enum RtdValue {
    Number(f64),
    Boolean(bool),
    Integer(i32),
    String(String),
    Error(ExcelErrorValue),
    Empty,
}

impl RtdValue {
    pub(crate) fn validate(&self) -> XllResult<()> {
        match self {
            Self::Number(value) if !value.is_finite() => Err(XllError::Domain {
                code: crate::DomainErrorCode::InvalidInput,
            }),
            Self::String(value) => {
                crate::utf16::encode_bounded(value, "RTD value", crate::utf16::EXCEL_STRING_LIMIT)?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

impl TryFrom<crate::OwnedExcelValue> for RtdValue {
    type Error = XllError;

    fn try_from(value: crate::OwnedExcelValue) -> XllResult<Self> {
        let value = match value {
            crate::OwnedExcelValue::Number(value) => Self::Number(value),
            crate::OwnedExcelValue::Boolean(value) => Self::Boolean(value),
            crate::OwnedExcelValue::Integer(value) => Self::Integer(value),
            crate::OwnedExcelValue::String(value) => Self::String(value),
            crate::OwnedExcelValue::Error(value) => Self::Error(value),
            crate::OwnedExcelValue::Missing | crate::OwnedExcelValue::Blank => Self::Empty,
            crate::OwnedExcelValue::Matrix(_) | crate::OwnedExcelValue::ArrayOutput(_) => {
                return Err(XllError::input(
                    "RTD value",
                    crate::InputError::Malformed("RTD values must be scalar"),
                ));
            }
        };
        value.validate()?;
        Ok(value)
    }
}

impl crate::IntoExcelValue for RtdValue {
    fn into_excel_value(self) -> XllResult<crate::OwnedExcelValue> {
        self.validate()?;
        Ok(match self {
            Self::Number(value) => crate::OwnedExcelValue::Number(value),
            Self::Boolean(value) => crate::OwnedExcelValue::Boolean(value),
            Self::Integer(value) => crate::OwnedExcelValue::Integer(value),
            Self::String(value) => crate::OwnedExcelValue::String(value),
            Self::Error(value) => crate::OwnedExcelValue::Error(value),
            Self::Empty => crate::OwnedExcelValue::Blank,
        })
    }
}

impl crate::ExcelReturn for RtdValue {
    type Output = Self;

    fn into_excel(self, _: &mut crate::ReturnContext<'_, '_>) -> XllResult<Self::Output> {
        Ok(self)
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
                code: crate::DomainErrorCode::InvalidInput,
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
                code: crate::DomainErrorCode::Overflow,
            })
        }
    }
}

impl IntoRtdValue for crate::ExcelSerialDate {
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
