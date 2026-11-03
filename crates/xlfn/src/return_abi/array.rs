use super::storage::ReturnStorage;
use crate::error::{DomainErrorCode, InputError};
use crate::value::{ExcelCellOutput, IntoExcel, MAX_ARRAY_BYTES, validate_matrix_dimensions};
use crate::{XllError, XllResult};
use xlfn_sys::{XLOPER12, XLOPER12Value, XLTYPE_STR};

/// A completed Excel array return value.
///
/// Values of this type are normally created by [`XlArrayBuilder::finish`].
pub struct XlArrayOutput {
    pub(crate) rows: usize,
    pub(crate) columns: usize,
    pub(crate) cells: Box<[XLOPER12]>,
    pub(crate) storage: Option<ReturnStorage>,
    pub(crate) payload_bytes: usize,
}

impl std::fmt::Debug for XlArrayOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("XlArrayOutput")
            .field("rows", &self.rows)
            .field("columns", &self.columns)
            .field("cells", &self.cells.len())
            .finish()
    }
}

/// Builds an Excel array return value incrementally.
///
/// This provides a low-allocation path for constructing large array results.
/// It encodes cells directly into its return representation instead of first
/// materializing a [`Matrix<T>`](crate::value::Matrix).
pub struct XlArrayBuilder {
    rows: usize,
    columns: usize,
    cells: Box<[std::mem::MaybeUninit<XLOPER12>]>,
    initialized: usize,
    storage: Option<ReturnStorage>,
    payload_bytes: usize,
}

impl XlArrayBuilder {
    /// Allocates an array builder after validating positive dimensions and payload limits.
    pub fn new(rows: usize, columns: usize) -> XllResult<Self> {
        let len = rows.checked_mul(columns).ok_or(XllError::Domain {
            code: DomainErrorCode::Overflow,
        })?;

        validate_matrix_dimensions(rows, columns, len)?;

        let cell_bytes =
            len.checked_mul(std::mem::size_of::<XLOPER12>())
                .ok_or(XllError::Domain {
                    code: DomainErrorCode::Overflow,
                })?;

        if cell_bytes > MAX_ARRAY_BYTES {
            return Err(XllError::input(
                "<array output>",
                InputError::TooLarge {
                    limit: MAX_ARRAY_BYTES,
                    actual: cell_bytes,
                },
            ));
        }

        Ok(Self {
            rows,
            columns,
            cells: Box::<[XLOPER12]>::new_uninit_slice(len),
            initialized: 0,
            storage: None,
            payload_bytes: cell_bytes,
        })
    }

    /// Encodes a complete row-major numeric slice into an array builder.
    ///
    /// Dimensions, the element count, and every number are validated. Cells
    /// are written directly into the return buffer without an intermediate
    /// [`Matrix<f64>`](crate::value::Matrix) or a capacity check per element.
    pub fn from_f64(rows: usize, columns: usize, values: &[f64]) -> XllResult<Self> {
        validate_matrix_dimensions(rows, columns, values.len())?;
        let mut builder = Self::new(rows, columns)?;
        for (cell, &value) in builder.cells.iter_mut().zip(values) {
            let value = crate::value::output::validate_number(value)?;
            cell.write(XLOPER12::number(value));
        }
        builder.initialized = values.len();
        Ok(builder)
    }

    #[inline]
    fn ensure_capacity(&self) -> XllResult<()> {
        if self.initialized == self.cells.len() {
            return Err(XllError::input(
                "<array output>",
                InputError::Malformed("too many array cells"),
            ));
        }

        Ok(())
    }

    #[inline]
    fn push_oper(&mut self, oper: XLOPER12) -> XllResult<()> {
        self.ensure_capacity()?;
        self.cells[self.initialized].write(oper);
        self.initialized += 1;

        Ok(())
    }

    /// Appends one finite number in row-major order.
    #[inline]
    pub fn push_f64(&mut self, value: f64) -> XllResult<()> {
        let value = crate::value::output::validate_number(value)?;
        self.push_oper(XLOPER12::number(value))
    }

    pub(crate) fn push_bool(&mut self, value: bool) -> XllResult<()> {
        self.push_oper(XLOPER12::boolean(value))
    }

    pub(crate) fn push_error(&mut self, value: crate::ExcelError) -> XllResult<()> {
        self.push_oper(XLOPER12::error(value.code()))
    }

    pub(crate) fn push_str(&mut self, text: &str) -> XllResult<()> {
        // A rejected push must not allocate unaccounted arena storage. Check
        // before encoding as well as at the final cell write.
        self.ensure_capacity()?;
        let utf16_length = crate::utf16::checked_utf16_len(
            text,
            "<array output>",
            crate::utf16::EXCEL_STRING_LIMIT,
        )?;
        let string_bytes = utf16_length
            .checked_add(1)
            .ok_or(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })?
            .checked_mul(std::mem::size_of::<u16>())
            .ok_or(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })?;

        let next_bytes = self
            .payload_bytes
            .checked_add(string_bytes)
            .ok_or(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })?;

        if next_bytes > MAX_ARRAY_BYTES {
            return Err(XllError::input(
                "<array output>",
                InputError::TooLarge {
                    limit: MAX_ARRAY_BYTES,
                    actual: next_bytes,
                },
            ));
        }

        let storage = self.storage.get_or_insert_with(ReturnStorage::new);
        let pointer = storage.alloc_counted_utf16_with_length(
            text,
            "<array output>",
            crate::utf16::EXCEL_STRING_LIMIT,
            utf16_length,
        )?;
        self.push_oper(XLOPER12 {
            value: XLOPER12Value { string: pointer },
            xltype: XLTYPE_STR,
        })?;

        self.payload_bytes = next_bytes;
        Ok(())
    }

    pub(crate) fn push_string(&mut self, text: String) -> XllResult<()> {
        self.push_str(&text)
    }

    pub(crate) fn push_cell(&mut self, value: ExcelCellOutput) -> XllResult<()> {
        match value {
            ExcelCellOutput::Number(value) => self.push_f64(value),
            ExcelCellOutput::Boolean(value) => self.push_oper(XLOPER12::boolean(value)),
            ExcelCellOutput::Error(error) => self.push_oper(XLOPER12::error(error.code())),
            ExcelCellOutput::String(value) => self.push_string(value),
        }
    }

    /// Encodes one output value into the next row-major cell.
    pub fn push<T: IntoExcel>(&mut self, value: T) -> XllResult<()> {
        self.ensure_capacity()?;
        value.write_into(self)
    }

    /// Completes the array, rejecting an incomplete cell count.
    pub fn finish(self) -> XllResult<XlArrayOutput> {
        let expected = self.rows * self.columns;

        if self.initialized != expected {
            return Err(XllError::ElementCountMismatch {
                rows: self.rows,
                columns: self.columns,
                expected,
                actual: self.initialized,
            });
        }

        // SAFETY: initialized == cells.len() so every element is written.
        let cells = unsafe { self.cells.assume_init() };

        Ok(XlArrayOutput {
            rows: self.rows,
            columns: self.columns,
            cells,
            storage: self.storage,
            payload_bytes: self.payload_bytes,
        })
    }
}

impl crate::value::output::ExcelCellSink for XlArrayBuilder {
    fn push_cell(&mut self, value: ExcelCellOutput) -> XllResult<()> {
        Self::push_cell(self, value)
    }

    fn push_f64(&mut self, value: f64) -> XllResult<()> {
        Self::push_f64(self, value)
    }

    fn push_bool(&mut self, value: bool) -> XllResult<()> {
        Self::push_bool(self, value)
    }

    fn push_str(&mut self, value: &str) -> XllResult<()> {
        Self::push_str(self, value)
    }

    fn push_string(&mut self, value: String) -> XllResult<()> {
        Self::push_string(self, value)
    }

    fn push_error(&mut self, value: crate::ExcelError) -> XllResult<()> {
        Self::push_error(self, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_slice_encoding_preserves_shape_and_values_without_string_storage() {
        let values = [-0.0, 0.0, 1.5, -2.5, f64::MAX, f64::MIN];
        let output = XlArrayBuilder::from_f64(2, 3, &values)
            .unwrap()
            .finish()
            .unwrap();
        assert_eq!((output.rows, output.columns), (2, 3));
        assert!(output.storage.is_none());
        for (cell, expected) in output.cells.iter().zip(values) {
            assert_eq!(cell.xltype, xlfn_sys::XLTYPE_NUM);
            // SAFETY: The checked tag selects the initialized numeric value.
            assert_eq!(unsafe { cell.value.number }.to_bits(), expected.to_bits());
        }
    }

    #[test]
    fn miri_numeric_push_preserves_error_order_and_partial_builder() {
        let mut builder = XlArrayBuilder::new(1, 2).unwrap();
        builder.push_f64(-0.0).unwrap();
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(
                builder.push_f64(value),
                Err(XllError::Domain {
                    code: DomainErrorCode::InvalidInput,
                })
            ));
        }
        assert_eq!(builder.initialized, 1);
        builder.push_f64(f64::MAX).unwrap();
        // Number validation precedes capacity rejection, including on a full builder.
        assert!(matches!(
            builder.push_f64(f64::NAN),
            Err(XllError::Domain {
                code: DomainErrorCode::InvalidInput,
            })
        ));
        assert!(matches!(
            builder.push_f64(1.0),
            Err(XllError::Input {
                reason: InputError::Malformed("too many array cells"),
                ..
            })
        ));
        let output = builder.finish().unwrap();
        for (cell, expected) in output.cells.iter().zip([-0.0, f64::MAX]) {
            assert_eq!(cell.xltype, xlfn_sys::XLTYPE_NUM);
            // SAFETY: the numeric tag selects the initialized number member.
            assert_eq!(unsafe { cell.value.number }.to_bits(), expected.to_bits());
        }
    }

    #[test]
    fn numeric_slice_encoding_rejects_shape_and_non_finite_values() {
        assert!(matches!(
            XlArrayBuilder::from_f64(1, 2, &[1.0]),
            Err(XllError::ElementCountMismatch { .. })
        ));
        assert!(XlArrayBuilder::from_f64(0, 1, &[]).is_err());
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(
                XlArrayBuilder::from_f64(1, 2, &[1.0, value]),
                Err(XllError::Domain {
                    code: DomainErrorCode::InvalidInput
                })
            ));
        }
        let valid = XlArrayBuilder::from_f64(1, 2, &[1.0, 2.0]).unwrap();
        assert!(valid.finish().is_ok());
    }

    #[test]
    fn miri_mixed_array_text_survives_arena_growth_and_owner_moves() {
        let texts = [String::new(), "日本語💡".to_owned(), "a".repeat(1_200)];
        let mut builder = XlArrayBuilder::new(1, 15).unwrap();
        for index in 0..15 {
            if index % 4 == 0 {
                builder.push_f64(index as f64).unwrap();
            } else {
                builder.push(texts[index % texts.len()].as_str()).unwrap();
            }
        }
        let outputs = Box::new([builder.finish().unwrap()]);
        for (index, cell) in outputs[0].cells.iter().enumerate() {
            let value = crate::value::XlValueRef::from_array_cell(cell).unwrap();
            if index % 4 == 0 {
                assert_eq!(value.as_f64().unwrap(), index as f64);
            } else {
                assert_eq!(
                    value.as_str().unwrap().try_to_string().unwrap(),
                    texts[index % texts.len()]
                );
            }
        }
    }

    #[test]
    fn rejected_string_push_does_not_create_storage() {
        let mut builder = XlArrayBuilder::new(1, 1).unwrap();
        builder.push(7.0).unwrap();
        let payload_bytes = builder.payload_bytes;
        assert!(builder.push_str("extra cell").is_err());
        assert!(builder.storage.is_none());
        assert_eq!(builder.payload_bytes, payload_bytes);
        assert_eq!(builder.initialized, 1);
        assert!(builder.finish().is_ok());
    }

    #[test]
    fn repeated_rejected_strings_leave_arena_and_payload_unchanged() {
        let mut builder = XlArrayBuilder::new(1, 1).unwrap();
        builder.push("first cell").unwrap();
        let arena_bytes = builder.storage.as_ref().unwrap().arena.allocated_bytes();
        let payload_bytes = builder.payload_bytes;
        let text = "x".repeat(crate::utf16::EXCEL_STRING_LIMIT);
        for _ in 0..8 {
            assert!(builder.push_str(&text).is_err());
        }
        assert_eq!(
            builder.storage.as_ref().unwrap().arena.allocated_bytes(),
            arena_bytes
        );
        assert_eq!(builder.payload_bytes, payload_bytes);
        assert_eq!(builder.initialized, 1);
        assert!(builder.finish().is_ok());
    }

    #[test]
    fn full_builder_does_not_invoke_custom_conversion() {
        struct Conversion<'a>(&'a std::cell::Cell<usize>);
        impl IntoExcel for Conversion<'_> {
            fn into_excel(self) -> XllResult<ExcelCellOutput> {
                self.0.set(self.0.get() + 1);
                Ok(ExcelCellOutput::Boolean(true))
            }
        }

        let converted = std::cell::Cell::new(0);
        let mut builder = XlArrayBuilder::new(1, 1).unwrap();
        builder.push(Conversion(&converted)).unwrap();
        assert_eq!(converted.get(), 1);
        assert!(builder.push(Conversion(&converted)).is_err());
        assert_eq!(converted.get(), 1);
    }
}
