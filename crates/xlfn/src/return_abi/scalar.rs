//! Owned scalar encoding completed while the Rust return value is live.

use super::storage::ReturnStorage;
use crate::error::InputError;
use crate::value::output::ExcelCellSink;
use crate::value::{ExcelCellOutput, IntoExcel};
use crate::{XllError, XllResult};
use xlfn_sys::{XLOPER12, XLOPER12Value, XLTYPE_STR};

/// An encoded scalar with ownership of any referenced UTF-16 storage.
///
/// No pointer into the Rust return value or call scratch is retained. The
/// owner may move out of the call scope before its root is published to Excel.
#[doc(hidden)]
pub struct XlScalarOutput {
    pub(super) oper: XLOPER12,
    pub(super) storage: Option<ReturnStorage>,
}

impl XlScalarOutput {
    #[cfg(test)]
    pub(crate) fn as_raw(&self) -> &XLOPER12 {
        &self.oper
    }

    #[inline]
    pub(crate) fn encode(value: impl IntoExcel) -> XllResult<Self> {
        let mut sink = ScalarSink { output: None };
        value.write_into(&mut sink)?;
        sink.output.ok_or_else(|| {
            XllError::input(
                "<return>",
                InputError::Malformed("scalar conversion produced no cell"),
            )
        })
    }

    fn from_oper(oper: XLOPER12) -> Self {
        Self {
            oper,
            storage: None,
        }
    }

    #[inline]
    fn from_str(text: &str) -> XllResult<Self> {
        let utf16_length =
            crate::utf16::checked_utf16_len(text, "<return>", crate::utf16::EXCEL_STRING_LIMIT)?;
        let Some(string_bytes) = utf16_length
            .checked_add(1)
            .and_then(|units| units.checked_mul(std::mem::size_of::<u16>()))
        else {
            return Err(XllError::Domain {
                code: crate::error::DomainErrorCode::Overflow,
            });
        };
        // A scalar has no separately allocated array cells. Avoid planning
        // an empty array and constructing errors on the ordinary text path.
        let Some(allocation_bytes) =
            std::mem::size_of::<super::ReturnBlock>().checked_add(string_bytes)
        else {
            return Err(XllError::Domain {
                code: crate::error::DomainErrorCode::Overflow,
            });
        };
        super::enforce_return_limit(allocation_bytes)?;
        let storage = ReturnStorage::new();
        let pointer = storage.alloc_counted_utf16_with_length(
            text,
            "<return>",
            crate::utf16::EXCEL_STRING_LIMIT,
            utf16_length,
        )?;
        Ok(Self {
            oper: XLOPER12 {
                value: XLOPER12Value { string: pointer },
                xltype: XLTYPE_STR,
            },
            storage: Some(storage),
        })
    }
}

/// The same primitive writer contract used by arrays, with exactly one cell.
struct ScalarSink {
    output: Option<XlScalarOutput>,
}

impl ScalarSink {
    #[inline]
    fn ensure_empty(&self) -> XllResult<()> {
        if self.output.is_some() {
            return Err(XllError::input(
                "<return>",
                InputError::Malformed("scalar conversion produced multiple cells"),
            ));
        }
        Ok(())
    }

    #[inline]
    fn push_oper(&mut self, oper: XLOPER12) -> XllResult<()> {
        self.ensure_empty()?;
        self.output = Some(XlScalarOutput::from_oper(oper));
        Ok(())
    }
}

impl ExcelCellSink for ScalarSink {
    #[inline]
    fn push_cell(&mut self, value: ExcelCellOutput) -> XllResult<()> {
        match value {
            ExcelCellOutput::Number(value) => self.push_f64(value),
            ExcelCellOutput::Boolean(value) => self.push_bool(value),
            ExcelCellOutput::Error(value) => self.push_error(value),
            ExcelCellOutput::String(value) => self.push_string(value),
        }
    }

    #[inline]
    fn push_f64(&mut self, value: f64) -> XllResult<()> {
        self.push_oper(XLOPER12::number(crate::value::output::validate_number(
            value,
        )?))
    }

    #[inline]
    fn push_bool(&mut self, value: bool) -> XllResult<()> {
        self.push_oper(XLOPER12::boolean(value))
    }

    #[inline]
    fn push_str(&mut self, value: &str) -> XllResult<()> {
        // Reject a second cell before counting or allocating its text.
        self.ensure_empty()?;
        self.output = Some(XlScalarOutput::from_str(value)?);
        Ok(())
    }

    #[inline]
    fn push_string(&mut self, value: String) -> XllResult<()> {
        self.push_str(&value)
    }

    #[inline]
    fn push_error(&mut self, value: crate::ExcelError) -> XllResult<()> {
        self.push_oper(XLOPER12::error(value.code()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call_return::{ExcelReturn, ReturnContext, ReturnPayload};
    use crate::value::XlValueRef;
    use std::cell::Cell;

    fn text(output: &XlScalarOutput) -> String {
        XlValueRef::from_array_cell(output.as_raw())
            .unwrap()
            .as_str()
            .unwrap()
            .try_to_string()
            .unwrap()
    }

    #[test]
    fn miri_scalar_text_outlives_call_scratch_and_owner_moves() {
        for value in ["", "ASCII", "日本語💡", &"日".repeat(1_200)] {
            let units: Vec<_> = value.encode_utf16().collect();
            let output = crate::call::with_excel_call_scope(|scope| {
                let borrowed = scope.scratch().decode_utf16(&units, "text").unwrap();
                ExcelReturn::into_excel(borrowed, &mut ReturnContext::new()).unwrap()
            });
            let ReturnPayload::Scalar(output) = output else {
                panic!("text must encode as a scalar");
            };
            let mut outputs = vec![output];
            let output = outputs.pop().unwrap();
            assert_eq!(text(&output), value);
            assert_eq!(output.as_raw().xltype, XLTYPE_STR);
        }
    }

    #[test]
    fn scalar_primitive_writer_and_enum_encode_owned_text() {
        struct Direct<'a>(&'a str);
        impl IntoExcel for Direct<'_> {
            fn into_excel(self) -> XllResult<ExcelCellOutput> {
                panic!("direct scalar writer must avoid semantic string ownership")
            }
            fn write_into<S: ExcelCellSink>(self, sink: &mut S) -> XllResult<()> {
                sink.push_str(self.0)
            }
        }
        let source = "日本語💡".to_owned();
        let ReturnPayload::Scalar(output) =
            ExcelReturn::into_excel(Direct(&source), &mut ReturnContext::new()).unwrap()
        else {
            panic!("direct text must encode as a scalar");
        };
        drop(source);
        assert_eq!(text(&output), "日本語💡");

        #[derive(crate::ExcelEnum)]
        enum Label {
            #[excel_enum(name = "価格💡")]
            Unicode,
        }
        let ReturnPayload::Scalar(output) =
            ExcelReturn::into_excel(Label::Unicode, &mut ReturnContext::new()).unwrap()
        else {
            panic!("enum label must encode as a scalar");
        };
        assert_eq!(text(&output), "価格💡");
    }

    #[test]
    fn custom_scalar_fallback_runs_once_and_preserves_errors() {
        struct Converted<'a>(&'a Cell<usize>, bool);
        impl IntoExcel for Converted<'_> {
            fn into_excel(self) -> XllResult<ExcelCellOutput> {
                self.0.set(self.0.get() + 1);
                if self.1 {
                    Ok(ExcelCellOutput::String("custom".to_owned()))
                } else {
                    Err(XllError::Domain {
                        code: crate::error::DomainErrorCode::NativeFailure,
                    })
                }
            }
        }
        let calls = Cell::new(0);
        let output = XlScalarOutput::encode(Converted(&calls, true)).unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(text(&output), "custom");
        assert!(matches!(
            XlScalarOutput::encode(Converted(&calls, false)),
            Err(XllError::Domain {
                code: crate::error::DomainErrorCode::NativeFailure,
            })
        ));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn scalar_sink_rejects_missing_and_extra_cells_without_extra_text_storage() {
        struct Empty;
        impl IntoExcel for Empty {
            fn into_excel(self) -> XllResult<ExcelCellOutput> {
                panic!("writer is selected");
            }
            fn write_into<S: ExcelCellSink>(self, _: &mut S) -> XllResult<()> {
                Ok(())
            }
        }
        assert!(matches!(
            XlScalarOutput::encode(Empty),
            Err(XllError::Input {
                argument: "<return>",
                reason: InputError::Malformed("scalar conversion produced no cell"),
            })
        ));
        let mut sink = ScalarSink { output: None };
        sink.push_str("first").unwrap();
        let before = sink
            .output
            .as_ref()
            .unwrap()
            .storage
            .as_ref()
            .unwrap()
            .arena
            .allocated_bytes();
        assert!(matches!(
            sink.push_str(&"x".repeat(crate::utf16::EXCEL_STRING_LIMIT)),
            Err(XllError::Input {
                argument: "<return>",
                reason: InputError::Malformed("scalar conversion produced multiple cells"),
            })
        ));
        assert_eq!(
            sink.output
                .as_ref()
                .unwrap()
                .storage
                .as_ref()
                .unwrap()
                .arena
                .allocated_bytes(),
            before
        );
        assert_eq!(text(sink.output.as_ref().unwrap()), "first");
    }

    #[test]
    fn scalar_encoding_preserves_number_bits_and_string_validation() {
        for value in [-0.0_f64, 0.0, 42.0] {
            let output = XlScalarOutput::encode(value).unwrap();
            assert!(output.storage.is_none());
            assert_eq!(
                XlValueRef::from_array_cell(output.as_raw())
                    .unwrap()
                    .as_f64()
                    .unwrap()
                    .to_bits(),
                value.to_bits()
            );
        }
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for result in [
                XlScalarOutput::encode(value),
                XlScalarOutput::encode(ExcelCellOutput::Number(value)),
            ] {
                assert!(matches!(
                    result,
                    Err(XllError::Domain {
                        code: crate::error::DomainErrorCode::InvalidInput
                    })
                ));
            }
        }
        let limit = crate::utf16::EXCEL_STRING_LIMIT;
        let at_limit = "日".repeat(limit);
        let output = XlScalarOutput::encode(at_limit.as_str()).unwrap();
        assert_eq!(text(&output), at_limit);
        for text in ["x".repeat(limit + 1), "日".repeat(limit + 1)] {
            assert!(
                matches!(XlScalarOutput::encode(text.as_str()), Err(XllError::Input {
                argument: "<return>", reason: InputError::TooLarge { limit: actual_limit, actual }
            }) if actual_limit == limit && actual == limit + 1)
            );
        }
    }
}
