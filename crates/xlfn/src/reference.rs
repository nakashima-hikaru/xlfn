use crate::{InputError, XlValueRef, XllError, XllResult};
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;
use std::slice;
use xlfn_sys::{IDSHEET, XLOPER12, XLREF12, XLTYPE_REF, XLTYPE_SREF};

const EXCEL_MAX_ROW: i32 = 1_048_575;
const EXCEL_MAX_COLUMN: i32 = 16_383;
const MAX_REFERENCE_AREAS: usize = 1_024;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SheetId(IDSHEET);

impl SheetId {
    #[must_use]
    pub const fn get(self) -> IDSHEET {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceArea {
    first_row: u32,
    last_row: u32,
    first_column: u32,
    last_column: u32,
}

impl ReferenceArea {
    fn parse(raw: XLREF12, argument: &'static str) -> XllResult<Self> {
        if raw.rw_first < 0
            || raw.rw_last < raw.rw_first
            || raw.rw_last > EXCEL_MAX_ROW
            || raw.col_first < 0
            || raw.col_last < raw.col_first
            || raw.col_last > EXCEL_MAX_COLUMN
        {
            return Err(XllError::input(
                argument,
                InputError::Malformed("invalid reference area"),
            ));
        }
        Ok(Self {
            first_row: raw.rw_first as u32,
            last_row: raw.rw_last as u32,
            first_column: raw.col_first as u32,
            last_column: raw.col_last as u32,
        })
    }

    #[must_use]
    pub const fn first_row(self) -> u32 {
        self.first_row
    }
    #[must_use]
    pub const fn last_row(self) -> u32 {
        self.last_row
    }
    #[must_use]
    pub const fn first_column(self) -> u32 {
        self.first_column
    }
    #[must_use]
    pub const fn last_column(self) -> u32 {
        self.last_column
    }
}

enum ReferenceKind<'call> {
    SameSheet(ReferenceArea),
    Sheet {
        sheet_id: SheetId,
        areas: &'call [XLREF12],
    },
}

/// A raw `U` reference valid only for the current Excel call.
pub struct ExcelReference<'call> {
    raw: &'call XLOPER12,
    kind: ReferenceKind<'call>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl ExcelReference<'_> {
    #[must_use]
    pub fn sheet_id(&self) -> Option<SheetId> {
        match self.kind {
            ReferenceKind::SameSheet(_) => None,
            ReferenceKind::Sheet { sheet_id, .. } => Some(sheet_id),
        }
    }

    #[must_use]
    pub fn is_multi_area(&self) -> bool {
        matches!(self.kind, ReferenceKind::Sheet { areas, .. } if areas.len() > 1)
    }

    #[must_use]
    pub fn areas(&self) -> ReferenceAreas<'_> {
        match &self.kind {
            ReferenceKind::SameSheet(area) => ReferenceAreas::One(Some(*area)),
            ReferenceKind::Sheet { areas, .. } => ReferenceAreas::Many(areas.iter()),
        }
    }

    pub(crate) fn raw_pointer(&self) -> NonNull<XLOPER12> {
        NonNull::from(self.raw)
    }
}

pub enum ReferenceAreas<'call> {
    One(Option<ReferenceArea>),
    Many(slice::Iter<'call, XLREF12>),
}

impl Iterator for ReferenceAreas<'_> {
    type Item = ReferenceArea;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::One(area) => area.take(),
            Self::Many(areas) => areas
                .next()
                .and_then(|area| ReferenceArea::parse(*area, "reference").ok()),
        }
    }
}

pub trait FromExcelReference<'call>: Sized {
    fn from_excel_reference(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self>;
}

impl<'call> FromExcelReference<'call> for ExcelReference<'call> {
    fn from_excel_reference(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        let kind = match value.base_type() {
            XLTYPE_SREF => {
                // SAFETY: xltypeSRef selects the sref union member.
                let sref = unsafe { value.raw().value.sref };
                if sref.count != 1 {
                    return Err(XllError::input(
                        argument,
                        InputError::Malformed("SRef count must be one"),
                    ));
                }
                ReferenceKind::SameSheet(ReferenceArea::parse(sref.reference, argument)?)
            }
            XLTYPE_REF => {
                // SAFETY: xltypeRef selects the mref union member.
                let mref = unsafe { value.raw().value.mref };
                // SAFETY: a valid xltypeRef contains a readable XLMREF12 table;
                // null is handled as an input error before dereferencing it.
                let table = unsafe { mref.references.as_ref() }
                    .ok_or_else(|| XllError::input(argument, InputError::NullPointer))?;
                let count = usize::from(table.count);
                if count == 0 || count > MAX_REFERENCE_AREAS {
                    return Err(XllError::input(
                        argument,
                        InputError::Malformed("invalid reference area count"),
                    ));
                }
                let first = table.reftbl.as_ptr();
                // SAFETY: Excel's variable-length XLMREF12 table contains count entries.
                let areas = unsafe { slice::from_raw_parts(first, count) };
                for area in areas {
                    ReferenceArea::parse(*area, argument)?;
                }
                ReferenceKind::Sheet {
                    sheet_id: SheetId(mref.sheet_id),
                    areas,
                }
            }
            _ => {
                return Err(XllError::input(
                    argument,
                    InputError::WrongType {
                        expected: "reference",
                        actual: value.base_type(),
                    },
                ));
            }
        };
        Ok(Self {
            raw: value.raw(),
            kind,
            _not_send_or_sync: PhantomData,
        })
    }
}

/// Converts one raw `U` argument at the generated ABI boundary.
///
/// # Safety
/// The pointer must remain live for `'call` and satisfy the XLOPER12 contract.
pub unsafe fn reference_from_raw<'call, T>(
    argument: &'static str,
    raw: *mut XLOPER12,
) -> XllResult<T>
where
    T: FromExcelReference<'call>,
{
    // SAFETY: The generated wrapper forwards Excel's live call argument.
    let borrowed = unsafe { XlValueRef::from_raw(raw) }?;
    T::from_excel_reference(borrowed, argument)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xlfn_sys::{XLOPER12SRef, XLOPER12Value};

    fn sref(area: XLREF12) -> XLOPER12 {
        XLOPER12 {
            value: XLOPER12Value {
                sref: XLOPER12SRef {
                    count: 1,
                    reference: area,
                },
            },
            xltype: XLTYPE_SREF,
        }
    }

    #[test]
    fn same_sheet_reference_preserves_inclusive_coordinates() {
        let mut raw = sref(XLREF12 {
            rw_first: 2,
            rw_last: 4,
            col_first: 1,
            col_last: 3,
        });
        // SAFETY: raw remains live for the reference and contains a valid SRef.
        let reference: ExcelReference<'_> =
            unsafe { reference_from_raw("cell", &mut raw) }.unwrap();
        assert_eq!(reference.sheet_id(), None);
        assert!(!reference.is_multi_area());
        assert_eq!(
            reference.areas().collect::<Vec<_>>(),
            vec![ReferenceArea {
                first_row: 2,
                last_row: 4,
                first_column: 1,
                last_column: 3,
            }]
        );
    }

    #[test]
    fn malformed_reference_area_is_rejected() {
        let mut raw = sref(XLREF12 {
            rw_first: 9,
            rw_last: 8,
            col_first: 0,
            col_last: 0,
        });
        // SAFETY: raw is structurally readable, and validation rejects its range.
        assert!(unsafe { reference_from_raw::<ExcelReference<'_>>("cell", &mut raw) }.is_err());
    }
}
