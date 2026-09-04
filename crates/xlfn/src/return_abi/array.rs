use super::storage::ReturnStorage;
use crate::error::{DomainErrorCode, InputError};
use crate::value::{ExcelCellOutput, IntoExcel, MAX_ARRAY_BYTES, validate_matrix_dimensions};
use crate::{XllError, XllResult};
use xlfn_sys::{XLOPER12, XLOPER12Value, XLTYPE_STR};

/// An Excel array whose cells are already encoded in their final ABI form.
///
/// Prefer constructing this through [`XlArrayBuilder`]. The return-value layer
/// adopts the cell allocation instead of materializing an intermediate semantic
/// value vector or encoding the array into a second cell buffer.
#[doc(hidden)]
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

/// Builds an Excel array directly in its final `XLOPER12` cell buffer.
///
/// This is the low-allocation output path for calculated arrays. The builder
/// owns exactly one cell buffer; returning the finished value transfers that
/// buffer to the DLL-owned return block without copying its cells.
pub struct XlArrayBuilder {
    rows: usize,
    columns: usize,
    cells: Box<[std::mem::MaybeUninit<XLOPER12>]>,
    initialized: usize,
    storage: Option<ReturnStorage>,
    payload_bytes: usize,
}

impl XlArrayBuilder {
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

    fn push_oper(&mut self, oper: XLOPER12) -> XllResult<()> {
        if self.initialized == self.cells.len() {
            return Err(XllError::input(
                "<array output>",
                InputError::Malformed("too many array cells"),
            ));
        }

        self.cells[self.initialized].write(oper);
        self.initialized += 1;

        Ok(())
    }

    #[allow(dead_code, reason = "Used by the unstable output API")]
    pub fn push_f64(&mut self, value: f64) -> XllResult<()> {
        if !value.is_finite() {
            return Err(XllError::input("<array output>", InputError::NonFinite));
        }
        self.push_oper(XLOPER12::number(value))
    }

    pub(crate) fn push_bool(&mut self, value: bool) -> XllResult<()> {
        self.push_oper(XLOPER12::boolean(value))
    }

    pub(crate) fn push_error(&mut self, value: crate::ExcelError) -> XllResult<()> {
        self.push_oper(XLOPER12::error(value.code()))
    }

    pub(crate) fn push_str(&mut self, text: &str) -> XllResult<()> {
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
            ExcelCellOutput::Number(value) if value.is_finite() => {
                self.push_oper(XLOPER12::number(value))
            }
            ExcelCellOutput::Number(_) => Err(XllError::input("<return>", InputError::NonFinite)),
            ExcelCellOutput::Boolean(value) => self.push_oper(XLOPER12::boolean(value)),
            ExcelCellOutput::Error(error) => self.push_oper(XLOPER12::error(error.code())),
            ExcelCellOutput::String(value) => self.push_string(value),
        }
    }

    pub fn push<T: IntoExcel>(&mut self, value: T) -> XllResult<()> {
        value.write_into(self)
    }

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
