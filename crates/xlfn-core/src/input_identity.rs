use crate::{InputError, XllError, XllResult};

const INLINE_ARGUMENT_BYTES: usize = 128;

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

enum ArgumentIdentity {
    Inline {
        bytes: [u8; INLINE_ARGUMENT_BYTES],
        len: usize,
    },
    Hashed([u8; 32]),
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

    fn finish(self) -> XllResult<ArgumentIdentity> {
        match self.error {
            Some(error) => Err(error),
            None => match self.sink {
                ArgumentSink::Inline { bytes, len } => Ok(ArgumentIdentity::Inline { bytes, len }),
                ArgumentSink::Hashed {
                    mut hasher,
                    buffer,
                    mut buffered,
                } => {
                    Self::flush_hashed(&mut hasher, &buffer, &mut buffered);
                    Ok(ArgumentIdentity::Hashed(*hasher.finalize().as_bytes()))
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

impl ArgumentIdentity {
    fn update_root(self, root: &mut blake3::Hasher) {
        match self {
            Self::Inline { bytes, len } => {
                root.update(&[ArgumentEncoding::Inline as u8]);
                root.update(&(len as u64).to_le_bytes());
                root.update(&bytes[..len]);
            }
            Self::Hashed(digest) => {
                root.update(&[ArgumentEncoding::Hashed as u8]);
                root.update(&digest);
            }
        }
    }
}

/// Builds one runtime-local input fingerprint.
pub(crate) struct InputFingerprintBuilder {
    root: blake3::Hasher,
}

impl InputFingerprintBuilder {
    pub(crate) fn new() -> Self {
        Self {
            root: blake3::Hasher::new(),
        }
    }

    pub(crate) fn with_argument<R>(
        &mut self,
        argument: &'static str,
        encode: impl FnOnce(&mut InputIdentityEncoder) -> XllResult<R>,
    ) -> XllResult<R> {
        let mut encoder = InputIdentityEncoder::new(argument);
        let value = encode(&mut encoder)?;
        let identity = encoder.finish()?;
        identity.update_root(&mut self.root);
        Ok(value)
    }

    pub(crate) fn finish(self) -> InputFingerprint {
        InputFingerprint::from_bytes(*self.root.finalize().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExcelCellValue, ExcelParameter, ExcelValue, Matrix, OptionalExcelValue};

    #[derive(Clone, Copy)]
    struct Pair(u32, u32);

    impl<'call> ExcelParameter<'call> for Pair {
        fn from_excel(
            _value: crate::XlValueRef<'call>,
            _argument: &'static str,
            _context: &crate::CallContext<'call>,
        ) -> XllResult<Self> {
            Err(XllError::input(
                "test",
                InputError::Malformed("test-only parameter"),
            ))
        }

        fn encode_identity(&self, encoder: &mut InputIdentityEncoder) {
            encoder.u32(self.0);
            encoder.u32(self.1);
        }
    }

    fn fingerprint<T>(values: &[T]) -> InputFingerprint
    where
        T: for<'call> ExcelParameter<'call>,
    {
        let mut builder = InputFingerprintBuilder::new();
        for value in values {
            builder
                .with_argument("arg", |encoder| {
                    value.encode_identity(encoder);
                    Ok(())
                })
                .unwrap();
        }
        builder.finish()
    }

    fn append_u64(bytes: &mut Vec<u8>, value: u64) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn append_string(bytes: &mut Vec<u8>, value: &str) {
        append_u64(bytes, value.len() as u64);
        bytes.extend_from_slice(value.as_bytes());
    }

    fn reference_fingerprint(arguments: &[&[u8]]) -> InputFingerprint {
        let mut root = blake3::Hasher::new();
        for argument in arguments {
            if argument.len() <= INLINE_ARGUMENT_BYTES {
                root.update(&[ArgumentEncoding::Inline as u8]);
                append_u64_stream(&mut root, argument.len() as u64);
                root.update(argument);
            } else {
                root.update(&[ArgumentEncoding::Hashed as u8]);
                root.update(blake3::hash(argument).as_bytes());
            }
        }
        InputFingerprint::from_bytes(*root.finalize().as_bytes())
    }

    fn append_u64_stream(hasher: &mut blake3::Hasher, value: u64) {
        hasher.update(&value.to_le_bytes());
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
            ArgumentIdentity::Hashed(_)
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
}
