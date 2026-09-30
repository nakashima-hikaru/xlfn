//! Benchmark input kinds and production preflight regression cases.
use super::*;

#[derive(Clone, Copy, Debug)]
pub enum InputKind {
    Number,
    Numbers,
    Strings,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Owned {
    Number(f64),
    Numbers(Matrix<f64>),
    Strings(Matrix<String>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn array_root(cells: &mut [XLOPER12]) -> XLOPER12 {
        XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                array: xlfn_sys::XLOPER12Array {
                    rows: cells.len() as i32,
                    columns: 1,
                    values: cells.as_mut_ptr(),
                },
            },
            xltype: xlfn_sys::XLTYPE_MULTI,
        }
    }

    fn numeric_matrix_identity(
        root: &XLOPER12,
        prepare: bool,
    ) -> XllResult<(Matrix<f64>, [u8; 32])> {
        crate::call::with_excel_call_scope_and_state(root, |root, scope| {
            let value = XlValueRef::from_array_cell(root)?;
            let mut arguments = input::ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let matrix = if prepare {
                arguments
                    .prepare::<Matrix<f64>>(0, "values", value)?
                    .materialize()?
            } else {
                arguments.decode::<Matrix<f64>>(0, "values", value)?
            };
            let identity = arguments.finish()?.expect("formula input identity");
            Ok((matrix, identity))
        })
    }

    fn check_numeric_matrix_identity(root: &XLOPER12) -> [u8; 32] {
        let (eager, eager_identity) = numeric_matrix_identity(root, false).unwrap();
        let (prepared, prepared_identity) = numeric_matrix_identity(root, true).unwrap();
        assert_eq!(
            (prepared.rows(), prepared.columns()),
            (eager.rows(), eager.columns()),
        );
        assert_eq!(prepared_identity, eager_identity);
        assert_eq!(
            prepared
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            eager
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
        );
        prepared_identity
    }

    #[test]
    fn miri_streamed_numeric_preparation_matches_eager_at_encoding_boundaries() {
        check_numeric_matrix_identity(&XLOPER12::number(-0.0));
        check_numeric_matrix_identity(&XLOPER12::integer(7));
        // Matrix dimensions use sixteen bytes before its values. Include the
        // inline and 4096-byte hash-buffer boundaries, plus ordinary sizes.
        let lengths: &[usize] = if cfg!(miri) {
            &[14, 15, 63, 64, 65]
        } else {
            &[1, 13, 14, 15, 16, 17, 63, 64, 65, 509, 510, 511, 512, 513]
        };
        for &length in lengths {
            let mut cells: Vec<_> = (0..length)
                .map(|index| match index % 3 {
                    0 => XLOPER12::number(-0.0),
                    1 => XLOPER12::integer(index as i32),
                    _ => XLOPER12::number(index as f64 * 0.125),
                })
                .collect();
            let root = array_root(&mut cells);
            let original = check_numeric_matrix_identity(&root);
            cells[length - 1] = XLOPER12::number(12_345.0);
            let root = array_root(&mut cells);
            assert_ne!(original, check_numeric_matrix_identity(&root));
        }
        let mut empty = [];
        let root = array_root(&mut empty);
        assert!(numeric_matrix_identity(&root, false).is_err());
        assert!(numeric_matrix_identity(&root, true).is_err());
    }

    #[test]
    fn streamed_numeric_preparation_preserves_integer_canonicalization_and_signed_zero() {
        let mut cells = [XLOPER12::integer(2), XLOPER12::number(-0.0)];
        let root = array_root(&mut cells);
        let integer_identity = check_numeric_matrix_identity(&root);
        cells[0] = XLOPER12::number(2.0);
        let root = array_root(&mut cells);
        assert_eq!(integer_identity, check_numeric_matrix_identity(&root));
        cells[1] = XLOPER12::number(0.0);
        let root = array_root(&mut cells);
        assert_ne!(integer_identity, check_numeric_matrix_identity(&root));
    }

    #[test]
    fn miri_streamed_numeric_preparation_keeps_late_validation_and_error_order() {
        let mut unknown_flag = XLOPER12::number(f64::NAN);
        unknown_flag.xltype |= 0x8000_0000;
        let mut unknown_type = XLOPER12::number(1.0);
        unknown_type.xltype = xlfn_sys::XLTYPE_NUM | xlfn_sys::XLTYPE_BOOL;
        let malformed_string = XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                string: std::ptr::null_mut(),
            },
            xltype: xlfn_sys::XLTYPE_STR,
        };
        let nested = array_root(&mut []);
        let lengths: &[usize] = if cfg!(miri) {
            &[1, 65]
        } else {
            &[1, 15, 16, 17, 63, 64, 65, 513]
        };
        for &length in lengths {
            for invalid in [
                XLOPER12::number(f64::NAN),
                XLOPER12::number(f64::INFINITY),
                unknown_flag,
                unknown_type,
                XLOPER12::boolean(true),
                XLOPER12::nil(),
                XLOPER12::missing(),
                XLOPER12::error(crate::ExcelError::DivisionByZero.code()),
                malformed_string,
                nested,
            ] {
                let mut cells = vec![XLOPER12::number(3.0); length];
                cells[length - 1] = invalid;
                let root = array_root(&mut cells);
                let eager = numeric_matrix_identity(&root, false).unwrap_err();
                let prepared = numeric_matrix_identity(&root, true).unwrap_err();
                assert_eq!(format!("{prepared:?}"), format!("{eager:?}"));
                crate::call::with_excel_call_scope_and_state(&root, |root, scope| {
                    let value = XlValueRef::from_array_cell(root).unwrap();
                    let context = CallContext::from_scope(scope);
                    let mut fingerprint = crate::input_identity::InputFingerprintBuilder::new(1);
                    // Even a caller that ignores the failed preparation cannot
                    // finalize the prefix as a valid formula input identity.
                    assert!(
                        fingerprint
                            .with_argument(0, "values", |identity| {
                                assert!(
                                    <Matrix<f64> as ExcelParameter<FormulaInputMode>>::prepare(
                                        value, "values", &context, identity,
                                    )
                                    .is_err()
                                );
                                Ok(())
                            })
                            .is_err()
                    );
                    assert!(fingerprint.finish().is_err());
                });
            }
        }
        let mut cells = [XLOPER12::number(f64::NAN), unknown_flag];
        let root = array_root(&mut cells);
        assert!(matches!(
            numeric_matrix_identity(&root, true),
            Err(XllError::Input {
                argument: "values",
                reason: InputError::NonFinite,
            }),
        ));
    }

    #[test]
    fn prepared_matrix_preserves_shape_integer_conversion_and_signed_zero() {
        let mut integer = XLOPER12::number(2.0);
        integer.xltype = xlfn_sys::XLTYPE_INT;
        integer.value = xlfn_sys::XLOPER12Value { integer: 2 };
        let mut cells = [XLOPER12::number(-0.0), integer];
        let root = XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                array: xlfn_sys::XLOPER12Array {
                    rows: 2,
                    columns: 1,
                    values: cells.as_mut_ptr(),
                },
            },
            xltype: xlfn_sys::XLTYPE_MULTI,
        };
        let mut encoder = InputIdentityEncoder::new("arg");
        crate::call::with_excel_call_scope_and_state(&root, |root, scope| {
            let raw = XlValueRef::from_array_cell(root).unwrap();
            let context = CallContext::from_scope(scope);
            let prepared = <Matrix<f64> as ExcelParameter<FormulaInputMode>>::prepare(
                raw,
                "arg",
                &context,
                &mut encoder,
            )
            .unwrap();
            let matrix = prepared.materialize().unwrap();
            assert_eq!((matrix.rows(), matrix.columns()), (2, 1));
            assert_eq!(matrix.as_slice()[0].to_bits(), (-0.0_f64).to_bits());
            assert_eq!(matrix.as_slice()[1], 2.0);
        });
    }

    #[test]
    fn streamed_string_identity_matches_owned_utf8_across_buffer_boundaries() {
        for text in [
            String::new(),
            "a".repeat(1000),
            "日本語💡".repeat(200),
            format!("{}é", "a".repeat(255)),
        ] {
            let units: Vec<_> = text.encode_utf16().collect();
            let mut eager = crate::input_identity::InputFingerprintBuilder::new(1);
            eager
                .with_argument(0, "arg", |identity| {
                    identity.string(&text);
                    Ok(())
                })
                .unwrap();
            let mut candidate = crate::input_identity::InputFingerprintBuilder::new(1);
            candidate
                .with_argument(0, "arg", |identity| identity.semantic_utf16(&units))
                .unwrap();
            assert_eq!(eager.finish().unwrap(), candidate.finish().unwrap());
        }
    }
}
