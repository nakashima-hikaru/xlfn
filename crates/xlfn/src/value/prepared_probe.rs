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

    fn numeric_matrix_ref_identity(
        root: &XLOPER12,
        prepare: bool,
    ) -> XllResult<(Matrix<f64>, [u8; 32])> {
        crate::call::with_excel_call_scope_and_state(root, |root, scope| {
            let value = XlValueRef::from_array_cell(root)?;
            let mut arguments = input::ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let matrix = if prepare {
                arguments
                    .prepare::<MatrixRef<'_, f64>>(0, "values", value)?
                    .materialize()?
            } else {
                arguments.decode::<MatrixRef<'_, f64>>(0, "values", value)?
            };
            let identity = arguments.finish()?.expect("formula input identity");
            Ok((matrix.to_owned()?, identity))
        })
    }

    #[test]
    fn miri_prepared_numeric_matrix_ref_defers_scratch_until_materialization() {
        let length = if cfg!(miri) { 65 } else { 100_000 };
        let mut cells = vec![XLOPER12::number(2.0); length];
        let root = array_root(&mut cells);
        crate::call::with_excel_call_scope_and_state(&root, |root, scope| {
            let value = XlValueRef::from_array_cell(root).unwrap();
            let mut arguments = input::ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let prepared = arguments
                .prepare::<MatrixRef<'_, f64>>(0, "values", value)
                .unwrap();
            arguments.finish().unwrap();
            assert_eq!(scope.scratch().allocated_bytes(), 0);
            // A warm lookup discards prepared state before invoking the UDF.
            drop(prepared);
            assert_eq!(scope.scratch().allocated_bytes(), 0);

            let mut arguments = input::ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let prepared = arguments
                .prepare::<MatrixRef<'_, f64>>(0, "values", value)
                .unwrap();
            arguments.finish().unwrap();
            let matrix = prepared.materialize().unwrap();
            assert_eq!((matrix.rows(), matrix.columns()), (length, 1));
            assert!(matrix.iter().all(|&value| value == 2.0));
            assert!(scope.scratch().allocated_bytes() >= length * std::mem::size_of::<f64>());
        });
    }

    #[test]
    fn prepared_numeric_matrix_ref_matches_eager_identity_and_values() {
        for length in [1, 14, 15, 16, 65, 510, 511, 513] {
            let mut cells: Vec<_> = (0..length)
                .map(|index| match index % 3 {
                    0 => XLOPER12::number(-0.0),
                    1 => XLOPER12::integer(index as i32),
                    _ => XLOPER12::number(index as f64 * 0.125),
                })
                .collect();
            let root = array_root(&mut cells);
            let (eager, eager_identity) = numeric_matrix_ref_identity(&root, false).unwrap();
            let (prepared, identity) = numeric_matrix_ref_identity(&root, true).unwrap();
            assert_eq!(identity, eager_identity);
            assert_eq!(identity, numeric_matrix_identity(&root, true).unwrap().1);
            assert_eq!(
                (prepared.rows(), prepared.columns()),
                (eager.rows(), eager.columns())
            );
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
            cells[length - 1] = XLOPER12::number(12_345.0);
            let root = array_root(&mut cells);
            assert_ne!(
                identity,
                numeric_matrix_ref_identity(&root, true).unwrap().1
            );
        }
        for root in [XLOPER12::number(-0.0), XLOPER12::integer(7)] {
            let (eager, eager_identity) = numeric_matrix_ref_identity(&root, false).unwrap();
            let (prepared, identity) = numeric_matrix_ref_identity(&root, true).unwrap();
            assert_eq!(identity, eager_identity);
            assert_eq!(
                prepared.as_slice()[0].to_bits(),
                eager.as_slice()[0].to_bits()
            );
        }
    }

    #[test]
    fn plain_numeric_matrix_ref_still_materializes_during_preparation() {
        let mut cells = [XLOPER12::number(1.0), XLOPER12::integer(2)];
        let root = array_root(&mut cells);
        crate::call::with_excel_call_scope_and_state(&root, |root, scope| {
            let mut arguments = input::ArgumentContext::<PlainInputMode>::from_scope(scope, 1);
            let value = XlValueRef::from_array_cell(root).unwrap();
            let prepared = arguments
                .prepare::<MatrixRef<'_, f64>>(0, "values", value)
                .unwrap();
            assert!(matches!(&prepared, PreparedArgument::Ready(_)));
            assert!(scope.scratch().allocated_bytes() >= 2 * std::mem::size_of::<f64>());
            assert_eq!(prepared.materialize().unwrap().as_slice(), &[1.0, 2.0]);
            assert_eq!(arguments.finish().unwrap(), None);
        });
    }

    #[test]
    fn prepared_matrix_ref_preserves_eager_policy_for_borrowed_text_and_custom_copy() {
        #[derive(Clone, Copy)]
        struct Custom(f64);
        impl<'call> FromExcel<'call> for Custom {
            fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
                f64::from_excel(value, argument).map(Self)
            }
        }
        impl ExcelInputIdentity for Custom {
            fn encode_input_identity(&self, identity: &mut InputIdentityEncoder) {
                identity.f64(self.0);
            }
        }
        impl<'call> PrepareExcel<'call> for Custom {
            type Prepared = Self;
            fn prepare(
                _: XlValueRef<'call>,
                _: &'static str,
                _: &mut InputIdentityEncoder,
            ) -> XllResult<Self::Prepared> {
                panic!("custom MatrixRef must retain its existing eager conversion")
            }
            fn materialize(_: Self::Prepared) -> XllResult<Self> {
                panic!("custom MatrixRef must already be materialized")
            }
        }
        let mut cells = [XLOPER12::number(1.0), XLOPER12::integer(2)];
        let root = array_root(&mut cells);
        crate::call::with_excel_call_scope_and_state(&root, |root, scope| {
            let mut arguments = input::ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let prepared = arguments
                .prepare::<MatrixRef<'_, Custom>>(
                    0,
                    "values",
                    XlValueRef::from_array_cell(root).unwrap(),
                )
                .unwrap();
            assert!(matches!(&prepared, PreparedArgument::Ready(_)));
            assert!(scope.scratch().allocated_bytes() > 0);
            assert_eq!(prepared.materialize().unwrap().as_slice()[1].0, 2.0);
            arguments.finish().unwrap();
        });
        let mut text = vec![2_u16, 'A' as u16, 'B' as u16];
        let mut cells = [XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                string: text.as_mut_ptr(),
            },
            xltype: xlfn_sys::XLTYPE_STR,
        }];
        let root = array_root(&mut cells);
        crate::call::with_excel_call_scope_and_state(&root, |root, scope| {
            let mut arguments = input::ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let prepared = arguments
                .prepare::<MatrixRef<'_, &str>>(
                    0,
                    "values",
                    XlValueRef::from_array_cell(root).unwrap(),
                )
                .unwrap();
            assert!(matches!(&prepared, PreparedArgument::Ready(_)));
            assert_eq!(prepared.materialize().unwrap().as_slice(), &["AB"]);
            arguments.finish().unwrap();
        });
    }

    #[test]
    fn prepared_matrix_ref_keeps_array_budget_errors_before_element_conversion() {
        #[derive(Clone, Copy, Debug)]
        #[repr(align(16384))]
        struct WideCell {
            _value: u8,
        }
        impl<'call> FromExcel<'call> for WideCell {
            fn from_excel(_: XlValueRef<'call>, _: &'static str) -> XllResult<Self> {
                panic!("array budget must reject before element conversion")
            }
        }
        impl ExcelInputIdentity for WideCell {
            fn encode_input_identity(&self, _: &mut InputIdentityEncoder) {
                panic!("array budget must reject before identity encoding")
            }
        }
        impl<'call> PrepareExcel<'call> for WideCell {
            type Prepared = ();
            fn prepare(
                _: XlValueRef<'call>,
                _: &'static str,
                _: &mut InputIdentityEncoder,
            ) -> XllResult<Self::Prepared> {
                panic!("array budget must reject before element preparation")
            }
            fn materialize(_: Self::Prepared) -> XllResult<Self> {
                panic!("array budget must reject before materialization")
            }
        }
        let length = MAX_ARRAY_BYTES
            / (std::mem::size_of::<WideCell>() + std::mem::size_of::<XLOPER12>())
            + 1;
        let mut cells = vec![XLOPER12::number(1.0); length];
        let root = array_root(&mut cells);
        crate::call::with_excel_call_scope_and_state(&root, |root, scope| {
            let value = XlValueRef::from_array_cell(root).unwrap();
            let mut eager = input::ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let eager_error = eager
                .decode::<MatrixRef<'_, WideCell>>(0, "values", value)
                .unwrap_err();
            let mut prepared = input::ArgumentContext::<FormulaInputMode>::from_scope(scope, 1);
            let prepared_error =
                match prepared.prepare::<MatrixRef<'_, WideCell>>(0, "values", value) {
                    Ok(_) => panic!("array budget must reject preparation"),
                    Err(error) => error,
                };
            assert!(matches!(
                prepared_error,
                XllError::Input {
                    argument: "values",
                    reason: InputError::TooLarge {
                        limit: MAX_ARRAY_BYTES,
                        ..
                    },
                }
            ));
            assert_eq!(format!("{prepared_error:?}"), format!("{eager_error:?}"));
            assert_eq!(scope.scratch().allocated_bytes(), 0);
            assert!(prepared.finish().is_err());
        });
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
                let borrowed_eager = numeric_matrix_ref_identity(&root, false).unwrap_err();
                let borrowed_prepared = numeric_matrix_ref_identity(&root, true).unwrap_err();
                assert_eq!(
                    format!("{borrowed_prepared:?}"),
                    format!("{borrowed_eager:?}")
                );
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
        assert!(matches!(
            numeric_matrix_ref_identity(&root, true),
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
