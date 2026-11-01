//! Dynamic borrowed input admission, lazy decoding, and identity regressions.

use super::*;
use crate::call::with_excel_call_scope_and_state;
use crate::input_identity::InputFingerprintBuilder;
use xlfn_sys::{XLOPER12Array, XLOPER12Value, XLTYPE_MULTI, XLTYPE_STR};

static_assertions::assert_not_impl_any!(ExcelValueRef<'static>: Send, Sync);
static_assertions::assert_not_impl_any!(ExcelArrayRef<'static>: Send, Sync);

fn text_raw(text: &mut [u16]) -> XLOPER12 {
    XLOPER12 {
        value: XLOPER12Value {
            string: text.as_mut_ptr(),
        },
        xltype: XLTYPE_STR,
    }
}

fn array_raw(cells: &mut [XLOPER12], rows: i32, columns: i32) -> XLOPER12 {
    XLOPER12 {
        value: XLOPER12Value {
            array: XLOPER12Array {
                values: cells.as_mut_ptr(),
                rows,
                columns,
            },
        },
        xltype: XLTYPE_MULTI,
    }
}

#[test]
fn borrowed_dynamic_array_decodes_only_accessed_cells_and_retains_prior_borrows() {
    let expected = "日本語💡".repeat(100);
    let mut text = std::iter::once(expected.encode_utf16().count() as u16)
        .chain(expected.encode_utf16())
        .collect::<Vec<_>>();
    let mut cells = [
        XLOPER12::number(7.0),
        text_raw(&mut text),
        XLOPER12::nil(),
        text_raw(&mut text),
    ];
    let raw = array_raw(&mut cells, 2, 2);
    with_excel_call_scope_and_state(&raw, |raw, scope| {
        let mut arguments = ArgumentContext::<PlainInputMode>::from_scope(scope, 1);
        let value = XlValueRef::from_array_cell(raw).unwrap();
        let ExcelValueRef::Array(array) = arguments
            .decode::<ExcelValueRef<'_>>(0, "values", value)
            .unwrap()
        else {
            panic!("expected dynamic array");
        };
        assert_eq!(
            array.shape(),
            crate::error::Shape {
                rows: 2,
                columns: 2
            }
        );
        assert_eq!((array.rows(), array.columns(), array.len()), (2, 2, 4));
        assert!(!array.is_empty());
        assert_eq!(scope.scratch().allocated_bytes(), 0);
        assert_eq!(array.get(0, 0).unwrap(), Some(ExcelCellRef::Number(7.0)));
        assert_eq!(array.get(1, 0).unwrap(), Some(ExcelCellRef::Blank));
        for (row, column) in [(2, 0), (0, 2), (usize::MAX, usize::MAX)] {
            assert_eq!(array.get(row, column).unwrap(), None);
        }
        assert_eq!(scope.scratch().allocated_bytes(), 0);
        let Some(ExcelCellRef::String(first)) = array.get(0, 1).unwrap() else {
            panic!("expected text cell");
        };
        assert!(scope.scratch().allocated_bytes() > 0);
        // Exercise arena growth and repeated decoding while retaining first.
        for _ in 0..20 {
            assert_eq!(
                array.get(1, 1).unwrap(),
                Some(ExcelCellRef::String(&expected))
            );
        }
        assert_eq!(first, expected);
    });
}

#[test]
fn borrowed_dynamic_preparation_matches_owned_identity_without_decoding_text() {
    let expected = "価格💡".repeat(100);
    let mut text = std::iter::once(expected.encode_utf16().count() as u16)
        .chain(expected.encode_utf16())
        .collect::<Vec<_>>();
    let mut cells = [
        XLOPER12::integer(10),
        XLOPER12::boolean(true),
        text_raw(&mut text),
        XLOPER12::error(ExcelError::NotAvailable.code()),
        XLOPER12::nil(),
        XLOPER12::number(-0.0),
    ];
    let array = array_raw(&mut cells, 2, 3);
    for raw in [
        XLOPER12::missing(),
        XLOPER12::nil(),
        XLOPER12::integer(10),
        XLOPER12::number(-0.0),
        XLOPER12::boolean(true),
        XLOPER12::error(ExcelError::NotAvailable.code()),
        text_raw(&mut text),
        array,
    ] {
        with_excel_call_scope_and_state(&raw, |raw, scope| {
            let value = XlValueRef::from_array_cell(raw).unwrap();
            let mut owned = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let _owned = owned.decode::<ExcelValue>(0, "values", value).unwrap();
            let expected_identity = owned.finish().unwrap();
            let mut borrowed = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let prepared = borrowed
                .prepare::<ExcelValueRef<'_>>(0, "values", value)
                .unwrap();
            assert_eq!(borrowed.finish().unwrap(), expected_identity);
            assert_eq!(scope.scratch().allocated_bytes(), 0);
            let decoded = prepared.materialize().unwrap();
            if matches!(decoded, ExcelValueRef::Array(_)) {
                assert_eq!(scope.scratch().allocated_bytes(), 0);
            }
            let mut builder = InputFingerprintBuilder::new(1);
            builder
                .with_argument(0, "values", |identity| {
                    <ExcelValueRef<'_> as ExcelParameter<FormulaInputMode>>::encode_decoded(
                        &decoded, identity,
                    );
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                Some(*builder.finish().unwrap().as_bytes()),
                expected_identity
            );
        });
    }
}

#[test]
fn borrowed_dynamic_scalar_preserves_missing_blank_and_empty_text() {
    let mut empty = [0_u16];
    for (raw, expected) in [
        (XLOPER12::missing(), None),
        (XLOPER12::nil(), Some(ExcelCellRef::Blank)),
        (text_raw(&mut empty), Some(ExcelCellRef::String(""))),
    ] {
        with_excel_call_scope_and_state(&raw, |raw, scope| {
            let value = XlValueRef::from_array_cell(raw).unwrap();
            let mut arguments = ArgumentContext::<PlainInputMode>::from_scope(scope, 1);
            let actual = arguments
                .decode::<ExcelValueRef<'_>>(0, "value", value)
                .unwrap();
            match actual {
                ExcelValueRef::Missing => assert_eq!(expected, None),
                ExcelValueRef::Scalar(cell) => assert_eq!(expected, Some(cell)),
                ExcelValueRef::Array(_) => panic!("unexpected array"),
            }
        });
    }
}

#[test]
fn borrowed_dynamic_rejects_unaccessed_bad_cells_during_decode_and_prepare() {
    let mut malformed_text = [1_u16, 0xd800];
    let mut null_text = XLOPER12 {
        value: XLOPER12Value {
            string: std::ptr::null_mut(),
        },
        xltype: XLTYPE_STR,
    };
    let malformed_header = XLOPER12 {
        value: XLOPER12Value { integer: 0 },
        xltype: 0x0003,
    };
    let malformed_string = text_raw(&mut malformed_text);
    let bad_cells = [
        text_raw(&mut malformed_text),
        null_text,
        XLOPER12::number(f64::NAN),
        XLOPER12::error(i32::MAX),
        XLOPER12::missing(),
        array_raw(std::slice::from_mut(&mut null_text), 1, 1),
        malformed_header,
    ];
    for mut cells in bad_cells
        .map(|bad| [XLOPER12::number(1.0), bad])
        .into_iter()
        .chain([
            [malformed_string, XLOPER12::number(f64::NAN)],
            [XLOPER12::number(f64::INFINITY), malformed_string],
            [malformed_string, malformed_header],
        ])
    {
        let raw = array_raw(&mut cells, 1, 2);
        with_excel_call_scope_and_state(&raw, |raw, scope| {
            let value = XlValueRef::from_array_cell(raw).unwrap();
            let mut owned = ArgumentContext::<PlainInputMode>::from_scope(scope, 1);
            let expected = owned.decode::<ExcelValue>(0, "values", value).unwrap_err();
            let mut plain = ArgumentContext::<PlainInputMode>::from_scope(scope, 1);
            let actual = plain
                .decode::<ExcelValueRef<'_>>(0, "values", value)
                .unwrap_err();
            assert_eq!(actual.to_string(), expected.to_string());
            let mut formula = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let actual = formula
                .prepare::<ExcelValueRef<'_>>(0, "values", value)
                .err()
                .unwrap();
            assert_eq!(actual.to_string(), expected.to_string());
            assert!(formula.finish().is_err());
            assert_eq!(scope.scratch().allocated_bytes(), 0);
        });
    }
}

#[test]
fn borrowed_dynamic_preserves_shape_and_aggregate_text_admission_limits() {
    let mut text = vec![b'a' as u16; crate::utf16::EXCEL_STRING_LIMIT + 1];
    text[0] = crate::utf16::EXCEL_STRING_LIMIT as u16;
    let per_cell = std::mem::size_of::<XLOPER12>()
        + std::mem::size_of::<ExcelCellValue>()
        + crate::utf16::EXCEL_STRING_LIMIT * 5;
    let count = MAX_ARRAY_BYTES / per_cell + 1;
    let mut cells = vec![text_raw(&mut text); count];
    for raw in [
        array_raw(&mut [], 0, 0),
        array_raw(&mut [], 1, 0),
        array_raw(&mut cells, count as i32, 1),
    ] {
        with_excel_call_scope_and_state(&raw, |raw, scope| {
            let value = XlValueRef::from_array_cell(raw).unwrap();
            let mut owned = ArgumentContext::<PlainInputMode>::from_scope(scope, 1);
            let expected = owned.decode::<ExcelValue>(0, "values", value).unwrap_err();
            let mut plain = ArgumentContext::<PlainInputMode>::from_scope(scope, 1);
            let actual = plain
                .decode::<ExcelValueRef<'_>>(0, "values", value)
                .unwrap_err();
            assert_eq!(actual.to_string(), expected.to_string());
            let mut formula = ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let actual = formula
                .prepare::<ExcelValueRef<'_>>(0, "values", value)
                .err()
                .unwrap();
            assert_eq!(actual.to_string(), expected.to_string());
            assert_eq!(scope.scratch().allocated_bytes(), 0);
        });
    }
}
