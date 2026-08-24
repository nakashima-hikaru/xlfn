use crate::error::InputError;
use crate::{XllError, XllResult};

const INLINE_ARGUMENT_BYTES: usize = 128;
const INPUT_FINGERPRINT_DOMAIN: &[u8] = b"xlfn-input-v2\0";

/// Runtime-local semantic identity of one UDF argument list.
///
/// This value is meaningful only together with the fixed UDF signature
/// identified by [`FormulaRevisionKey::udf_id`](crate::handle::FormulaRevisionKey).
/// It is not a stable, serialized, or cross-version identifier.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct InputFingerprint([u8; 32]);

impl InputFingerprint {
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[repr(u8)]
enum ArgumentEncoding {
    Inline = 0,
    Hashed = 1,
}

/// Encodes the semantic identity of one converted Excel argument.
///
/// Implementations of [`ExcelParameter`] encode every value that is observable
/// through the Rust parameter type. Small arguments are kept inline and large
/// arguments promote to a digest without changing the encoded bytes.
#[allow(
    clippy::large_enum_variant,
    reason = "keep the inline path small while retaining a stack hasher and staging buffer for large arguments"
)]
enum ArgumentSink {
    Inline {
        bytes: [u8; INLINE_ARGUMENT_BYTES],
        len: usize,
    },
    Hashed {
        hasher: blake3::Hasher,
        buffer: [u8; INLINE_ARGUMENT_BYTES],
        buffered: usize,
    },
}

#[cfg(test)]
enum ArgumentIdentity {
    Inline { len: usize },
    Hashed,
}

pub struct InputIdentityEncoder {
    sink: ArgumentSink,
    error: Option<XllError>,
    argument: &'static str,
}

impl InputIdentityEncoder {
    pub(crate) fn new(argument: &'static str) -> Self {
        Self {
            sink: ArgumentSink::Inline {
                bytes: [0; INLINE_ARGUMENT_BYTES],
                len: 0,
            },
            error: None,
            argument,
        }
    }

    /// Adds a caller-defined one-byte variant tag.
    pub fn tag(&mut self, tag: u8) {
        self.write(&[tag]);
    }

    /// Adds length-delimited bytes.
    pub fn bytes(&mut self, bytes: &[u8]) {
        self.u64(bytes.len() as u64);
        self.write(bytes);
    }

    /// Adds a UTF-8 string by its bytes, not by its source Excel encoding.
    pub fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    /// Adds a boolean value.
    pub fn bool(&mut self, value: bool) {
        self.write(&[u8::from(value)]);
    }

    /// Adds an `f64` using its converted Rust bit pattern.
    pub fn f64(&mut self, value: f64) {
        self.u64(value.to_bits());
    }

    /// Adds a little-endian `u32`.
    pub fn u32(&mut self, value: u32) {
        self.write(&value.to_le_bytes());
    }

    /// Adds a little-endian `u64`.
    pub fn u64(&mut self, value: u64) {
        self.write(&value.to_le_bytes());
    }

    /// Adds a little-endian signed `i64`.
    pub fn i64(&mut self, value: i64) {
        self.write(&value.to_le_bytes());
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.error.is_some() {
            return;
        }

        let needs_promotion = match &self.sink {
            ArgumentSink::Inline { len, .. } => len
                .checked_add(bytes.len())
                .is_none_or(|total| total > INLINE_ARGUMENT_BYTES),
            ArgumentSink::Hashed { .. } => false,
        };
        if needs_promotion {
            self.promote_to_hashed();
        }

        match &mut self.sink {
            ArgumentSink::Inline { bytes: buffer, len } => {
                buffer[*len..*len + bytes.len()].copy_from_slice(bytes);
                *len += bytes.len();
            }
            ArgumentSink::Hashed {
                hasher,
                buffer,
                buffered,
            } => Self::write_hashed(hasher, buffer, buffered, bytes),
        }
    }

    fn promote_to_hashed(&mut self) {
        let previous = std::mem::replace(
            &mut self.sink,
            ArgumentSink::Hashed {
                hasher: blake3::Hasher::new(),
                buffer: [0; INLINE_ARGUMENT_BYTES],
                buffered: 0,
            },
        );
        let ArgumentSink::Inline { bytes, len } = previous else {
            unreachable!("identity encoder promotes only from inline mode");
        };
        let ArgumentSink::Hashed { hasher, .. } = &mut self.sink else {
            unreachable!("identity encoder promotion must create a hashed sink");
        };
        hasher.update(&bytes[..len]);
    }

    fn write_hashed(
        hasher: &mut blake3::Hasher,
        buffer: &mut [u8; INLINE_ARGUMENT_BYTES],
        buffered: &mut usize,
        bytes: &[u8],
    ) {
        if bytes.len() >= INLINE_ARGUMENT_BYTES {
            Self::flush_hashed(hasher, buffer, buffered);
            hasher.update(bytes);
            return;
        }
        if *buffered + bytes.len() > INLINE_ARGUMENT_BYTES {
            Self::flush_hashed(hasher, buffer, buffered);
        }
        buffer[*buffered..*buffered + bytes.len()].copy_from_slice(bytes);
        *buffered += bytes.len();
    }

    fn flush_hashed(
        hasher: &mut blake3::Hasher,
        buffer: &[u8; INLINE_ARGUMENT_BYTES],
        buffered: &mut usize,
    ) {
        if *buffered == 0 {
            return;
        }
        hasher.update(&buffer[..*buffered]);
        *buffered = 0;
    }

    fn finish_into(self, root: &mut blake3::Hasher) -> XllResult<()> {
        match self.error {
            Some(error) => Err(error),
            None => match self.sink {
                ArgumentSink::Inline { bytes, len } => {
                    root.update(&[ArgumentEncoding::Inline as u8]);
                    root.update(&(len as u64).to_le_bytes());
                    root.update(&bytes[..len]);
                    Ok(())
                }
                ArgumentSink::Hashed {
                    mut hasher,
                    buffer,
                    mut buffered,
                } => {
                    Self::flush_hashed(&mut hasher, &buffer, &mut buffered);
                    root.update(&[ArgumentEncoding::Hashed as u8]);
                    root.update(hasher.finalize().as_bytes());
                    Ok(())
                }
            },
        }
    }

    #[cfg(test)]
    fn finish(self) -> XllResult<ArgumentIdentity> {
        match self.error {
            Some(error) => Err(error),
            None => match self.sink {
                ArgumentSink::Inline { len, .. } => Ok(ArgumentIdentity::Inline { len }),
                ArgumentSink::Hashed {
                    mut hasher,
                    buffer,
                    mut buffered,
                } => {
                    Self::flush_hashed(&mut hasher, &buffer, &mut buffered);
                    let _ = hasher.finalize();
                    Ok(ArgumentIdentity::Hashed)
                }
            },
        }
    }

    pub(crate) const fn argument(&self) -> &'static str {
        self.argument
    }

    pub(crate) fn fail(&mut self, error: XllError) {
        let error = match error {
            XllError::Input { reason, .. } => XllError::Input {
                argument: self.argument,
                reason,
            },
            other => other,
        };
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    pub(crate) fn fail_input(&mut self, reason: InputError) {
        self.fail(XllError::input(self.argument, reason));
    }
}

/// Builds one runtime-local input fingerprint.
#[doc(hidden)]
pub struct InputFingerprintBuilder {
    root: blake3::Hasher,
    expected_arguments: usize,
    next_argument: usize,
}

impl InputFingerprintBuilder {
    pub(crate) fn new(expected_arguments: usize) -> Self {
        let mut root = blake3::Hasher::new();
        root.update(INPUT_FINGERPRINT_DOMAIN);
        root.update(&(expected_arguments as u64).to_le_bytes());
        Self {
            root,
            expected_arguments,
            next_argument: 0,
        }
    }

    pub(crate) fn with_argument<R>(
        &mut self,
        index: usize,
        argument: &'static str,
        encode: impl FnOnce(&mut InputIdentityEncoder) -> XllResult<R>,
    ) -> XllResult<R> {
        if index != self.next_argument {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
            });
        }
        self.root.update(&[0xA0]);
        self.root.update(&(index as u64).to_le_bytes());
        self.root.update(&(argument.len() as u64).to_le_bytes());
        self.root.update(argument.as_bytes());
        let mut encoder = InputIdentityEncoder::new(argument);
        let value = encode(&mut encoder)?;
        encoder.finish_into(&mut self.root)?;
        self.next_argument += 1;
        Ok(value)
    }

    pub(crate) fn finish(self) -> XllResult<InputFingerprint> {
        if self.next_argument != self.expected_arguments {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::INPUT_FINGERPRINT,
            });
        }
        Ok(InputFingerprint::from_bytes(
            *self.root.finalize().as_bytes(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{ExcelCellValue, ExcelValue, Matrix, OptionalExcelValue};

    #[derive(Clone, Copy)]
    struct Pair(u32, u32);

    trait TestIdentity {
        fn encode_identity(&self, encoder: &mut InputIdentityEncoder);
    }

    impl TestIdentity for Pair {
        fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
            encoder.u32(self.0);
            encoder.u32(self.1);
        }
    }

    impl TestIdentity for f64 {
        fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
            encoder.f64(*self);
        }
    }

    impl TestIdentity for String {
        fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
            encoder.string(self);
        }
    }

    impl TestIdentity for Option<f64> {
        fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
            match self {
                None => encoder.bool(false),
                Some(value) => {
                    encoder.bool(true);
                    value.encode_identity(encoder);
                }
            }
        }
    }

    impl TestIdentity for OptionalExcelValue<f64> {
        fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
            match self {
                Self::Missing => encoder.tag(0),
                Self::Blank => encoder.tag(1),
                Self::Value(value) => {
                    encoder.tag(2);
                    value.encode_identity(encoder);
                }
            }
        }
    }

    impl TestIdentity for Vec<f64> {
        fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
            encoder.u64(self.len() as u64);
            for value in self {
                value.encode_identity(encoder);
            }
        }
    }

    impl TestIdentity for ExcelCellValue {
        fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
            match self {
                Self::Number(value) => {
                    encoder.tag(1);
                    value.encode_identity(encoder);
                }
                Self::Boolean(value) => {
                    encoder.tag(2);
                    encoder.bool(*value);
                }
                Self::String(value) => {
                    encoder.tag(3);
                    encoder.string(value);
                }
                Self::Error(value) => {
                    encoder.tag(4);
                    encoder.i64(i64::from(value.code()));
                }
                Self::Blank => encoder.tag(5),
            }
        }
    }

    impl<T: TestIdentity> TestIdentity for Matrix<T> {
        fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
            encoder.u64(self.rows() as u64);
            encoder.u64(self.columns() as u64);
            for value in self.as_slice() {
                value.encode_identity(encoder);
            }
        }
    }

    impl TestIdentity for ExcelValue {
        fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
            match self {
                Self::Scalar(ExcelCellValue::Number(value)) => {
                    encoder.tag(1);
                    encoder.tag(1);
                    value.encode_identity(encoder);
                }
                Self::Missing => encoder.tag(2),
                Self::Array(value) => {
                    encoder.tag(3);
                    value.encode_identity(encoder);
                }
                _ => encoder.tag(1),
            }
        }
    }

    fn fingerprint<T>(values: &[T]) -> InputFingerprint
    where
        T: TestIdentity,
    {
        let mut builder = InputFingerprintBuilder::new(values.len());
        for (index, value) in values.iter().enumerate() {
            builder
                .with_argument(index, "arg", |encoder| {
                    value.encode_identity(encoder);
                    Ok(())
                })
                .unwrap();
        }
        builder.finish().unwrap()
    }

    fn append_u64(bytes: &mut Vec<u8>, value: u64) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn append_string(bytes: &mut Vec<u8>, value: &str) {
        append_u64(bytes, value.len() as u64);
        bytes.extend_from_slice(value.as_bytes());
    }

    fn reference_fingerprint(arguments: &[&[u8]]) -> InputFingerprint {
        let mut builder = InputFingerprintBuilder::new(arguments.len());
        for (index, argument) in arguments.iter().enumerate() {
            builder
                .with_argument(index, "arg", |encoder| {
                    encoder.write(argument);
                    Ok(())
                })
                .unwrap();
        }
        builder.finish().unwrap()
    }

    #[test]
    fn fingerprint_matches_reference_for_builtin_values() {
        let number = 42.0_f64;
        let mut number_payload = Vec::new();
        append_u64(&mut number_payload, number.to_bits());
        assert_eq!(
            fingerprint(&[number]),
            reference_fingerprint(&[number_payload.as_slice()]),
        );

        let string = String::from("short");
        let mut string_payload = Vec::new();
        append_string(&mut string_payload, &string);
        assert_eq!(
            fingerprint(std::slice::from_ref(&string)),
            reference_fingerprint(&[string_payload.as_slice()]),
        );

        let optional = Some(42.0_f64);
        let mut optional_payload = vec![1];
        append_u64(&mut optional_payload, number.to_bits());
        assert_eq!(
            fingerprint(std::slice::from_ref(&optional)),
            reference_fingerprint(&[optional_payload.as_slice()]),
        );

        let optional_excel = OptionalExcelValue::Value(number);
        let mut optional_excel_payload = vec![2];
        append_u64(&mut optional_excel_payload, number.to_bits());
        assert_eq!(
            fingerprint(std::slice::from_ref(&optional_excel)),
            reference_fingerprint(&[optional_excel_payload.as_slice()]),
        );

        let values = vec![number, 7.0];
        let mut values_payload = Vec::new();
        append_u64(&mut values_payload, values.len() as u64);
        for value in &values {
            append_u64(&mut values_payload, value.to_bits());
        }
        assert_eq!(
            fingerprint(std::slice::from_ref(&values)),
            reference_fingerprint(&[values_payload.as_slice()]),
        );

        let matrix = Matrix::new(1, 2, values.clone()).unwrap();
        let mut matrix_payload = Vec::new();
        append_u64(&mut matrix_payload, matrix.rows() as u64);
        append_u64(&mut matrix_payload, matrix.columns() as u64);
        for value in matrix.as_slice() {
            append_u64(&mut matrix_payload, value.to_bits());
        }
        assert_eq!(
            fingerprint(std::slice::from_ref(&matrix)),
            reference_fingerprint(&[matrix_payload.as_slice()]),
        );

        let owned = ExcelValue::Scalar(ExcelCellValue::Number(number));
        let mut owned_payload = vec![1, 1];
        append_u64(&mut owned_payload, number.to_bits());
        assert_eq!(
            fingerprint(std::slice::from_ref(&owned)),
            reference_fingerprint(&[owned_payload.as_slice()]),
        );
    }

    #[test]
    fn fingerprint_matches_reference_for_custom_parameters() {
        let pair = Pair(7, 11);
        let mut payload = Vec::new();
        payload.extend_from_slice(&pair.0.to_le_bytes());
        payload.extend_from_slice(&pair.1.to_le_bytes());
        assert_eq!(
            fingerprint(std::slice::from_ref(&pair)),
            reference_fingerprint(&[payload.as_slice()]),
        );
    }

    #[test]
    fn inline_argument_boundary_is_128_bytes() {
        let mut encoder_127 = InputIdentityEncoder::new("arg");
        encoder_127.write(&[0; 127]);
        assert!(matches!(
            encoder_127.finish().unwrap(),
            ArgumentIdentity::Inline { len: 127, .. }
        ));

        let mut encoder_128 = InputIdentityEncoder::new("arg");
        encoder_128.write(&[0; 128]);
        assert!(matches!(
            encoder_128.finish().unwrap(),
            ArgumentIdentity::Inline { len: 128, .. }
        ));

        let mut encoder_129 = InputIdentityEncoder::new("arg");
        encoder_129.write(&[0; 129]);
        assert!(matches!(
            encoder_129.finish().unwrap(),
            ArgumentIdentity::Hashed
        ));
    }

    #[test]
    fn large_arguments_use_hashed_framing() {
        let values: Vec<f64> = (0..32).map(|value| value as f64).collect();
        let mut payload = Vec::new();
        append_u64(&mut payload, values.len() as u64);
        for value in &values {
            append_u64(&mut payload, value.to_bits());
        }
        assert_eq!(
            fingerprint(std::slice::from_ref(&values)),
            reference_fingerprint(&[payload.as_slice()]),
        );
    }

    #[test]
    fn argument_boundaries_are_part_of_the_root_fingerprint() {
        let pair = Pair(1, 2);
        assert_ne!(fingerprint(&[pair]), fingerprint(&[Pair(1, 2), Pair(0, 0)]));
    }

    #[test]
    fn f64_preserves_the_converted_bit_pattern() {
        assert_ne!(fingerprint(&[-0.0]), fingerprint(&[0.0]));
    }

    #[test]
    fn optional_states_and_matrix_shape_remain_semantic() {
        assert_ne!(
            fingerprint(&[OptionalExcelValue::<f64>::Missing]),
            fingerprint(&[OptionalExcelValue::<f64>::Blank]),
        );

        let row = Matrix::new(1, 2, vec![1.0, 2.0]).unwrap();
        let column = Matrix::new(2, 1, vec![1.0, 2.0]).unwrap();
        assert_ne!(fingerprint(&[row]), fingerprint(&[column]));
    }

    #[test]
    fn excel_value_presence_variants_remain_distinct() {
        assert_ne!(
            fingerprint(&[ExcelValue::Scalar(ExcelCellValue::Number(1.0))]),
            fingerprint(&[ExcelValue::Missing]),
        );
    }

    #[test]
    fn fingerprint_builder_rejects_incomplete_or_out_of_order_arguments() {
        let mut incomplete = InputFingerprintBuilder::new(2);
        incomplete.with_argument(0, "first", |_| Ok(())).unwrap();
        assert!(incomplete.finish().is_err());

        let mut out_of_order = InputFingerprintBuilder::new(2);
        assert!(out_of_order.with_argument(1, "second", |_| Ok(())).is_err());
        out_of_order.with_argument(0, "first", |_| Ok(())).unwrap();
        out_of_order.with_argument(1, "second", |_| Ok(())).unwrap();
        assert!(out_of_order.finish().is_ok());
    }
}
