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
