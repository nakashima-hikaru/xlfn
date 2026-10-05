use crate::error::InputError;
use crate::value::{XlValueRef, XlValueType};
use crate::{XllError, XllResult};
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;
use std::slice;
use xlfn_sys::{IDSHEET, XLOPER12, XLREF12};

const EXCEL_MAX_ROW: i32 = 1_048_575;
const EXCEL_MAX_COLUMN: i32 = 16_383;
const MAX_REFERENCE_AREAS: usize = 1_024;

/// Opaque identity of an Excel sheet supplied by a reference argument.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SheetId(IDSHEET);

impl SheetId {
    #[must_use]
    /// Returns the host-provided numeric identity for correlation.
    pub const fn get(self) -> usize {
        self.0
    }
}

use core::range::RangeInclusive;

/// Validated rectangular reference area with zero-based inclusive coordinates.
///
/// Its [`Display`](std::fmt::Display) representation is a sheet-local A1 address
/// without a sheet name or absolute-reference markers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceArea {
    rows: RangeInclusive<u32>,
    columns: RangeInclusive<u32>,
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
            rows: RangeInclusive {
                start: raw.rw_first as u32,
                last: raw.rw_last as u32,
            },
            columns: RangeInclusive {
                start: raw.col_first as u32,
                last: raw.col_last as u32,
            },
        })
    }

    #[must_use]
    /// Returns the first zero-based row in the area.
    pub const fn first_row(self) -> u32 {
        self.rows.start
    }
    #[must_use]
    /// Returns the last zero-based row in the area.
    pub const fn last_row(self) -> u32 {
        self.rows.last
    }
    #[must_use]
    /// Returns the first zero-based column in the area.
    pub const fn first_column(self) -> u32 {
        self.columns.start
    }
    #[must_use]
    /// Returns the last zero-based column in the area.
    pub const fn last_column(self) -> u32 {
        self.columns.last
    }
    /// Returns the number of rows, including both endpoints.
    #[must_use]
    pub const fn row_count(self) -> u32 {
        self.rows.last - self.rows.start + 1
    }

    /// Returns the number of columns, including both endpoints.
    #[must_use]
    pub const fn column_count(self) -> u32 {
        self.columns.last - self.columns.start + 1
    }

    /// Returns the number of cells, including both endpoints.
    ///
    /// The result is `u64` so a full worksheet also fits on 32-bit hosts.
    #[must_use]
    pub const fn cell_count(self) -> u64 {
        self.row_count() as u64 * self.column_count() as u64
    }

    #[must_use]
    /// Returns the zero-based inclusive row range.
    pub const fn rows(self) -> RangeInclusive<u32> {
        self.rows
    }
    #[must_use]
    /// Returns the zero-based inclusive column range.
    pub const fn columns(self) -> RangeInclusive<u32> {
        self.columns
    }
}

impl std::fmt::Display for ReferenceArea {
    /// Formats a sheet-local A1 address, for example `A1` or `A1:B10`.
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn cell(output: &mut std::fmt::Formatter<'_>, row: u32, column: u32) -> std::fmt::Result {
            let mut number = column + 1;
            let mut label = [0_u8; 3];
            let mut start = label.len();
            while number != 0 {
                start -= 1;
                label[start] = b'A' + ((number - 1) % 26) as u8;
                number = (number - 1) / 26;
            }
            let label = std::str::from_utf8(&label[start..]).expect("A1 column contains ASCII");
            write!(output, "{label}{}", row + 1)
        }
        cell(output, self.first_row(), self.first_column())?;
        if self.row_count() != 1 || self.column_count() != 1 {
            output.write_str(":")?;
            cell(output, self.last_row(), self.last_column())?;
        }
        Ok(())
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
    /// Returns an explicit sheet identity, or `None` for a current-sheet reference.
    pub fn sheet_id(&self) -> Option<SheetId> {
        match self.kind {
            ReferenceKind::SameSheet(_) => None,
            ReferenceKind::Sheet { sheet_id, .. } => Some(sheet_id),
        }
    }

    #[must_use]
    /// Returns whether the reference contains more than one rectangular area.
    pub fn is_multi_area(&self) -> bool {
        matches!(self.kind, ReferenceKind::Sheet { areas, .. } if areas.len() > 1)
    }

    #[must_use]
    /// Iterates over all validated rectangular areas in host order.
    pub fn areas(&self) -> ReferenceAreas<'_> {
        ReferenceAreas {
            inner: match &self.kind {
                ReferenceKind::SameSheet(area) => ReferenceAreasInner::One(Some(*area)),
                ReferenceKind::Sheet { areas, .. } => ReferenceAreasInner::Many(areas.iter()),
            },
        }
    }

    pub(crate) fn raw_pointer(&self) -> NonNull<XLOPER12> {
        NonNull::from_ref(self.raw)
    }
}

/// Iterates over the validated areas of an Excel reference.
pub struct ReferenceAreas<'call> {
    inner: ReferenceAreasInner<'call>,
}

enum ReferenceAreasInner<'call> {
    One(Option<ReferenceArea>),
    Many(slice::Iter<'call, XLREF12>),
}

impl Iterator for ReferenceAreas<'_> {
    type Item = ReferenceArea;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            ReferenceAreasInner::One(area) => area.take(),
            ReferenceAreasInner::Many(areas) => areas.next().map(|area| ReferenceArea {
                // All coordinates were validated before this iterator was exposed.
                rows: RangeInclusive {
                    start: area.rw_first as u32,
                    last: area.rw_last as u32,
                },
                columns: RangeInclusive {
                    start: area.col_first as u32,
                    last: area.col_last as u32,
                },
            }),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = match &self.inner {
            ReferenceAreasInner::One(area) => usize::from(area.is_some()),
            ReferenceAreasInner::Many(areas) => areas.len(),
        };
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for ReferenceAreas<'_> {}
impl std::iter::FusedIterator for ReferenceAreas<'_> {}

/// Converts a call-scoped reference argument into an application type.
///
/// `#[excel_arg(reference)]` dispatches to the declared parameter type's
/// implementation. Delegate to [`ExcelReference::from_excel_reference`] to
/// validate the reference, then retain its call-scoped view or copy bounded
/// metadata into an owned value. The argument still requires macro-sheet
/// execution and does not participate in formula-revision input identity.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be constructed from an Excel cell reference",
    label = "`{Self}` does not implement `FromExcelReference`",
    note = "implement `FromExcelReference` for `{Self}` to accept reference arguments"
)]
pub trait FromExcelReference<'call>: Sized {
    /// Converts and validates a call-scoped reference, naming errors with `argument`.
    fn from_excel_reference(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self>;
}

impl<'call> FromExcelReference<'call> for ExcelReference<'call> {
    fn from_excel_reference(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        let kind = match value.value_type() {
            XlValueType::SimpleReference => {
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
            XlValueType::Reference => {
                // SAFETY: xltypeRef selects the mref union member.
                let mref = unsafe { value.raw().value.mref };
                let table = NonNull::new(mref.references)
                    .ok_or_else(|| XllError::input(argument, InputError::NullPointer))?;
                // SAFETY: a valid xltypeRef contains a readable XLMREF12 header.
                let count = usize::from(unsafe { (*table.as_ptr()).count });
                if count == 0 || count > MAX_REFERENCE_AREAS {
                    return Err(XllError::input(
                        argument,
                        InputError::Malformed("invalid reference area count"),
                    ));
                }
                // Keep the host allocation's provenance. Borrowing `reftbl`
                // would restrict the pointer to the ABI's one-element tail,
                // while the actual variable-length table can contain more.
                // SAFETY: the non-null table points to the live host allocation.
                let first = unsafe { (&raw const (*table.as_ptr()).reftbl).cast::<XLREF12>() };
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
            actual => {
                return Err(XllError::input(
                    argument,
                    InputError::WrongType {
                        expected: "reference",
                        actual,
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
pub(crate) unsafe fn reference_from_raw<'call, T>(
    argument: &'static str,
    raw: *mut XLOPER12,
) -> XllResult<T>
where
    T: FromExcelReference<'call>,
{
    // SAFETY: The generated wrapper forwards Excel's live call argument.
    let borrowed = unsafe { XlValueRef::from_raw(raw) }.map_err(|error| match error {
        XllError::Input { reason, .. } => XllError::input(argument, reason),
        other => other,
    })?;
    T::from_excel_reference(borrowed, argument)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xlfn_sys::{XLOPER12SRef, XLOPER12Value, XLTYPE_SREF};

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
                rows: RangeInclusive { start: 2, last: 4 },
                columns: RangeInclusive { start: 1, last: 3 },
            }]
        );
    }

    #[test]
    fn area_geometry_and_a1_cover_column_and_sheet_boundaries() {
        for (row, column, expected) in [
            (0, 0, "A1"),
            (9, 25, "Z10"),
            (0, 26, "AA1"),
            (0, 701, "ZZ1"),
            (0, 702, "AAA1"),
            (1_048_575, 16_383, "XFD1048576"),
        ] {
            let area = ReferenceArea::parse(
                XLREF12 {
                    rw_first: row,
                    rw_last: row,
                    col_first: column,
                    col_last: column,
                },
                "area",
            )
            .unwrap();
            assert_eq!(area.to_string(), expected);
            assert_eq!(
                (area.row_count(), area.column_count(), area.cell_count()),
                (1, 1, 1)
            );
        }
        let area = ReferenceArea::parse(
            XLREF12 {
                rw_first: 0,
                rw_last: EXCEL_MAX_ROW,
                col_first: 0,
                col_last: EXCEL_MAX_COLUMN,
            },
            "area",
        )
        .unwrap();
        assert_eq!(area.to_string(), "A1:XFD1048576");
        assert_eq!(area.cell_count(), 17_179_869_184);
    }

    #[test]
    fn reference_area_iterator_has_an_exact_remaining_length() {
        let mut raw = sref(XLREF12 {
            rw_first: 0,
            rw_last: 9,
            col_first: 0,
            col_last: 1,
        });
        // SAFETY: raw remains live and unchanged for the call-scoped reference.
        let reference: ExcelReference<'_> =
            unsafe { reference_from_raw("range", &mut raw) }.unwrap();
        let mut areas = reference.areas();
        assert_eq!(areas.len(), 1);
        assert_eq!(areas.size_hint(), (1, Some(1)));
        assert_eq!(areas.next().unwrap().to_string(), "A1:B10");
        assert_eq!(areas.len(), 0);
        assert_eq!(areas.size_hint(), (0, Some(0)));
        assert_eq!(areas.next(), None);
        assert_eq!(areas.next(), None);
    }

    #[test]
    fn malformed_reference_tag_preserves_the_argument_name() {
        let mut raw = sref(XLREF12 {
            rw_first: 0,
            rw_last: 0,
            col_first: 0,
            col_last: 0,
        });
        raw.xltype |= 0x2000;
        // SAFETY: the SRef remains readable; admission rejects the unknown flag.
        let result = unsafe { reference_from_raw::<ExcelReference<'_>>("source_range", &mut raw) };
        assert!(matches!(
            result,
            Err(XllError::Input {
                argument: "source_range",
                reason: InputError::Malformed("unknown xltype flag")
            })
        ));
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

    #[test]
    fn miri_multi_area_reference_preserves_the_full_table() {
        #[repr(C)]
        struct ReferenceTable {
            count: u16,
            areas: [XLREF12; 2],
        }

        assert_eq!(
            std::mem::offset_of!(ReferenceTable, areas),
            std::mem::offset_of!(xlfn_sys::XLMREF12, reftbl),
        );
        let mut table = ReferenceTable {
            count: 2,
            areas: [
                XLREF12 {
                    rw_first: 2,
                    rw_last: 4,
                    col_first: 1,
                    col_last: 3,
                },
                XLREF12 {
                    rw_first: 8,
                    rw_last: 9,
                    col_first: 5,
                    col_last: 7,
                },
            ],
        };
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                mref: xlfn_sys::XLOPER12MRef {
                    references: (&raw mut table).cast(),
                    sheet_id: 42,
                },
            },
            xltype: xlfn_sys::XLTYPE_REF,
        };
        // SAFETY: raw and the two-area table remain live and unchanged for the borrow.
        let reference: ExcelReference<'_> =
            unsafe { reference_from_raw("areas", &mut raw) }.unwrap();
        assert_eq!(reference.sheet_id().unwrap().get(), 42);
        assert!(reference.is_multi_area());
        let mut iter = reference.areas();
        assert_eq!(iter.len(), 2);
        let first = iter.next().unwrap();
        assert_eq!(iter.len(), 1);
        let second = iter.next().unwrap();
        assert_eq!(iter.len(), 0);
        assert_eq!(iter.next(), None);
        let areas = [first, second];
        assert_eq!(areas.len(), 2);
        assert_eq!((areas[0].first_row(), areas[0].last_row()), (2, 4));
        assert_eq!((areas[1].first_row(), areas[1].last_row()), (8, 9));
        assert_eq!((areas[1].first_column(), areas[1].last_column()), (5, 7));
    }
    #[test]
    fn malformed_later_area_rejects_the_entire_reference() {
        #[repr(C)]
        struct ReferenceTable {
            count: u16,
            areas: [XLREF12; 2],
        }
        let valid = XLREF12 {
            rw_first: 0,
            rw_last: 0,
            col_first: 0,
            col_last: 0,
        };
        let mut table = ReferenceTable {
            count: 2,
            areas: [
                valid,
                XLREF12 {
                    rw_first: 1,
                    rw_last: 0,
                    ..valid
                },
            ],
        };
        let mut raw = XLOPER12 {
            value: XLOPER12Value {
                mref: xlfn_sys::XLOPER12MRef {
                    references: (&raw mut table).cast(),
                    sheet_id: 42,
                },
            },
            xltype: xlfn_sys::XLTYPE_REF,
        };
        // SAFETY: the two-entry table is readable; admission must reject its second area.
        let result = unsafe { reference_from_raw::<ExcelReference<'_>>("range", &mut raw) };
        assert!(matches!(
            result,
            Err(XllError::Input {
                argument: "range",
                reason: InputError::Malformed("invalid reference area")
            })
        ));
    }
}
