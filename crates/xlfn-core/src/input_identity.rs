use crate::{DomainErrorCode, ExcelParameter, InputError, XllError, XllResult};

const MAX_INPUT_IDENTITY_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const ARGUMENT_DOMAIN: &[u8] = b"xlfn-input-argument-v5\0";
pub(crate) const ROOT_DOMAIN: &[u8] = b"xlfn-input-fingerprint-v5\0";
// Part of the v5 wire schema. Changing this value requires v6.
const INLINE_ARGUMENT_BYTES: usize = 128;
pub(crate) const INLINE_ARGUMENT_MODE: u8 = 0;
pub(crate) const HASHED_ARGUMENT_MODE: u8 = 1;
const ROOT_PREFIX_BYTES: usize = 8 + ROOT_DOMAIN.len() + 8;

/// The fixed-size semantic identity of one converted Excel argument list.
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

/// Encodes the semantic identity of one converted Excel argument.
///
/// Implementations of [`ExcelParameter`] encode every value that is observable
/// through the Rust parameter type. The trait's associated domain is written
/// by [`InputFingerprintBuilder`], so an implementation cannot accidentally
/// omit its top-level type separator. Small arguments are kept inline and
/// large arguments promote to a digest without changing the encoded bytes.
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
    bytes: usize,
    error: Option<XllError>,
}

impl InputIdentityEncoder {
    pub(crate) fn new() -> Self {
        Self {
            sink: ArgumentSink::Inline {
                bytes: [0; INLINE_ARGUMENT_BYTES],
                len: 0,
            },
            bytes: 0,
            error: None,
        }
    }

    /// Adds a length-prefixed nested stable domain separator for a semantic
    /// type.
    pub fn domain(&mut self, domain: &[u8]) {
        self.u64(domain.len() as u64);
        self.write(domain);
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

        let actual = match self.bytes.checked_add(bytes.len()) {
            Some(actual) => actual,
            None => {
                self.error = Some(XllError::Domain {
                    code: DomainErrorCode::Overflow,
                });
                return;
            }
        };
        if actual > MAX_INPUT_IDENTITY_BYTES {
            self.error = Some(XllError::input(
                "input_identity",
                InputError::TooLarge {
                    limit: MAX_INPUT_IDENTITY_BYTES,
                    actual,
                },
            ));
            return;
        }

        self.bytes = actual;

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

    pub(crate) fn fail(&mut self, error: XllError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    pub(crate) const fn bytes_written(&self) -> usize {
        self.bytes
    }
}

/// Builds one input fingerprint without allocating a collection of
/// per-argument digests. The argument count is known by the generated wrapper,
/// so the root hash can be initialized with its final framing immediately.
pub(crate) struct InputFingerprintBuilder {
    root: blake3::Hasher,
    expected_arguments: usize,
    recorded_arguments: usize,
    bytes: usize,
}

impl InputFingerprintBuilder {
    pub(crate) fn new(expected_arguments: usize) -> Self {
        let mut root = blake3::Hasher::new();
        let mut prefix = [0_u8; ROOT_PREFIX_BYTES];
        prefix[..8].copy_from_slice(&(ROOT_DOMAIN.len() as u64).to_le_bytes());
        prefix[8..8 + ROOT_DOMAIN.len()].copy_from_slice(ROOT_DOMAIN);
        prefix[8 + ROOT_DOMAIN.len()..].copy_from_slice(&(expected_arguments as u64).to_le_bytes());
        root.update(&prefix);
        Self {
            root,
            expected_arguments,
            recorded_arguments: 0,
            bytes: 0,
        }
    }

    pub(crate) fn with_argument<'call, T, R, F>(&mut self, encode: F) -> XllResult<R>
    where
        T: ExcelParameter<'call>,
        F: FnOnce(&mut InputIdentityEncoder) -> XllResult<R>,
    {
        if self.recorded_arguments >= self.expected_arguments {
            return Err(XllError::input(
                "input_identity",
                InputError::Malformed("too many arguments recorded"),
            ));
        }

        let mut encoder = InputIdentityEncoder::new();
        encoder.domain(ARGUMENT_DOMAIN);
        encoder.domain(T::IDENTITY_DOMAIN);
        let value = encode(&mut encoder)?;
        let encoded_bytes = encoder.bytes_written();
        let identity = encoder.finish()?;
        let actual = self
            .bytes
            .checked_add(encoded_bytes)
            .ok_or(XllError::Domain {
                code: DomainErrorCode::Overflow,
            })?;
        if actual > MAX_INPUT_IDENTITY_BYTES {
            return Err(XllError::input(
                "input_identity",
                InputError::TooLarge {
                    limit: MAX_INPUT_IDENTITY_BYTES,
                    actual,
                },
            ));
        }
        self.bytes = actual;
        match identity {
            ArgumentIdentity::Inline { bytes, len } => {
                self.root.update(&[INLINE_ARGUMENT_MODE]);
                self.root.update(&(len as u64).to_le_bytes());
                self.root.update(&bytes[..len]);
            }
            ArgumentIdentity::Hashed(digest) => {
                self.root.update(&[HASHED_ARGUMENT_MODE]);
                self.root.update(&digest);
            }
        }
        self.recorded_arguments += 1;
        Ok(value)
    }

    pub(crate) fn finish(self) -> XllResult<InputFingerprint> {
        if self.recorded_arguments != self.expected_arguments {
            return Err(XllError::input(
                "input_identity",
                InputError::Malformed("argument count mismatch"),
            ));
        }
        Ok(InputFingerprint::from_bytes(
            *self.root.finalize().as_bytes(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExcelParameter, Matrix, OptionalExcelValue, OwnedExcelValue};

    #[derive(Clone, Copy)]
    struct Pair(u32, u32);

    impl<'call> ExcelParameter<'call> for Pair {
        const IDENTITY_DOMAIN: &'static [u8] = b"test.pair.v1";

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
        let mut builder = InputFingerprintBuilder::new(values.len());
        for value in values {
            builder
                .with_argument::<T, (), _>(|encoder| {
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

    fn append_domain(bytes: &mut Vec<u8>, domain: &[u8]) {
        append_u64(bytes, domain.len() as u64);
        bytes.extend_from_slice(domain);
    }

    fn append_string(bytes: &mut Vec<u8>, value: &str) {
        append_u64(bytes, value.len() as u64);
        bytes.extend_from_slice(value.as_bytes());
    }

    fn reference_fingerprint<F>(domain: &[u8], encode: F) -> InputFingerprint
    where
        F: FnOnce(&mut Vec<u8>),
    {
        let mut argument = Vec::new();
        append_domain(&mut argument, ARGUMENT_DOMAIN);
        append_domain(&mut argument, domain);
        encode(&mut argument);

        let mut root = blake3::Hasher::new();
        append_domain_stream(&mut root, ROOT_DOMAIN);
        append_u64_stream(&mut root, 1);
        if argument.len() <= INLINE_ARGUMENT_BYTES {
            root.update(&[INLINE_ARGUMENT_MODE]);
            append_u64_stream(&mut root, argument.len() as u64);
            root.update(&argument);
        } else {
            root.update(&[HASHED_ARGUMENT_MODE]);
            root.update(blake3::hash(&argument).as_bytes());
        }
        InputFingerprint::from_bytes(*root.finalize().as_bytes())
    }

    fn append_u64_stream(hasher: &mut blake3::Hasher, value: u64) {
        hasher.update(&value.to_le_bytes());
    }

    fn append_domain_stream(hasher: &mut blake3::Hasher, domain: &[u8]) {
        append_u64_stream(hasher, domain.len() as u64);
        hasher.update(domain);
    }

    #[test]
    fn v5_fingerprint_matches_reference_for_builtin_values() {
        let number = 42.0_f64;
        assert_eq!(
            fingerprint(&[number]),
            reference_fingerprint(f64::IDENTITY_DOMAIN, |bytes| append_u64(
                bytes,
                number.to_bits()
            )),
        );

        let string = String::from("short");
        assert_eq!(
            fingerprint(std::slice::from_ref(&string)),
            reference_fingerprint(String::IDENTITY_DOMAIN, |bytes| append_string(
                bytes, &string
            )),
        );

        let optional = Some(42.0_f64);
        assert_eq!(
            fingerprint(std::slice::from_ref(&optional)),
            reference_fingerprint(<Option<f64>>::IDENTITY_DOMAIN, |bytes| {
                append_domain(bytes, f64::IDENTITY_DOMAIN);
                bytes.push(1);
                append_u64(bytes, number.to_bits());
            }),
        );

        let optional_excel = OptionalExcelValue::Value(number);
        assert_eq!(
            fingerprint(std::slice::from_ref(&optional_excel)),
            reference_fingerprint(<OptionalExcelValue<f64>>::IDENTITY_DOMAIN, |bytes| {
                append_domain(bytes, f64::IDENTITY_DOMAIN);
                bytes.push(2);
                append_u64(bytes, number.to_bits());
            }),
        );

        let values = vec![number, 7.0];
        assert_eq!(
            fingerprint(std::slice::from_ref(&values)),
            reference_fingerprint(<Vec<f64>>::IDENTITY_DOMAIN, |bytes| {
                append_domain(bytes, f64::IDENTITY_DOMAIN);
                append_u64(bytes, values.len() as u64);
                for value in &values {
                    append_u64(bytes, value.to_bits());
                }
            }),
        );

        let matrix = Matrix::new(1, 2, values.clone()).unwrap();
        assert_eq!(
            fingerprint(std::slice::from_ref(&matrix)),
            reference_fingerprint(<Matrix<f64>>::IDENTITY_DOMAIN, |bytes| {
                append_domain(bytes, f64::IDENTITY_DOMAIN);
                append_u64(bytes, matrix.rows() as u64);
                append_u64(bytes, matrix.columns() as u64);
                for value in matrix.as_slice() {
                    append_u64(bytes, value.to_bits());
                }
            }),
        );

        let owned = OwnedExcelValue::Number(number);
        assert_eq!(
            fingerprint(std::slice::from_ref(&owned)),
            reference_fingerprint(OwnedExcelValue::IDENTITY_DOMAIN, |bytes| {
                bytes.push(0);
                append_u64(bytes, number.to_bits());
            }),
        );
    }

    #[test]
    fn v5_fingerprint_matches_reference_for_custom_parameters() {
        let pair = Pair(7, 11);
        assert_eq!(
            fingerprint(std::slice::from_ref(&pair)),
            reference_fingerprint(Pair::IDENTITY_DOMAIN, |bytes| {
                bytes.extend_from_slice(&pair.0.to_le_bytes());
                bytes.extend_from_slice(&pair.1.to_le_bytes());
            }),
        );
    }

    #[test]
    fn v5_inline_argument_boundary_is_128_bytes() {
        let mut encoder_127 = InputIdentityEncoder::new();
        encoder_127.write(&[0; 127]);
        assert!(matches!(
            encoder_127.finish().unwrap(),
            ArgumentIdentity::Inline { len: 127, .. }
        ));

        let mut encoder_128 = InputIdentityEncoder::new();
        encoder_128.write(&[0; 128]);
        assert!(matches!(
            encoder_128.finish().unwrap(),
            ArgumentIdentity::Inline { len: 128, .. }
        ));

        let mut encoder_129 = InputIdentityEncoder::new();
        encoder_129.write(&[0; 129]);
        assert!(matches!(
            encoder_129.finish().unwrap(),
            ArgumentIdentity::Hashed(_)
        ));
    }

    #[test]
    fn large_arguments_use_the_hashed_v5_framing() {
        let values: Vec<f64> = (0..32).map(|value| value as f64).collect();
        assert_eq!(
            fingerprint(std::slice::from_ref(&values)),
            reference_fingerprint(<Vec<f64>>::IDENTITY_DOMAIN, |bytes| {
                append_domain(bytes, f64::IDENTITY_DOMAIN);
                append_u64(bytes, values.len() as u64);
                for value in &values {
                    append_u64(bytes, value.to_bits());
                }
            }),
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
    fn optional_presence_and_matrix_shape_are_semantic_identity() {
        assert_ne!(
            fingerprint(&[OptionalExcelValue::<f64>::Missing]),
            fingerprint(&[OptionalExcelValue::<f64>::Blank]),
        );

        let row = Matrix::new(1, 2, vec![1.0, 2.0]).unwrap();
        let column = Matrix::new(2, 1, vec![1.0, 2.0]).unwrap();
        assert_ne!(fingerprint(&[row]), fingerprint(&[column]));
    }

    #[test]
    fn container_element_domain_is_encoded_once() {
        let values = vec![1.0_f64, 2.0, 3.0];
        let actual = fingerprint(std::slice::from_ref(&values));
        let expected = reference_fingerprint(<Vec<f64>>::IDENTITY_DOMAIN, |bytes| {
            append_domain(bytes, f64::IDENTITY_DOMAIN);
            append_u64(bytes, values.len() as u64);
            for value in &values {
                append_u64(bytes, value.to_bits());
            }
        });

        assert_eq!(actual, expected);
    }

    #[test]
    fn domain_framing_separates_namespace_from_payload() {
        fn digest(domain: &[u8], payload: &[u8]) -> InputFingerprint {
            reference_fingerprint(domain, |bytes| {
                append_u64(bytes, payload.len() as u64);
                bytes.extend_from_slice(payload);
            })
        }

        assert_ne!(digest(b"ab", b"c"), digest(b"a", b"bc"));
    }

    #[test]
    fn too_large_identity_is_rejected() {
        let mut builder = InputFingerprintBuilder::new(1);
        let result = builder.with_argument::<Pair, _, _>(|encoder| {
            encoder.bytes(&vec![0_u8; MAX_INPUT_IDENTITY_BYTES]);
            Ok(Pair(1, 2))
        });
        assert!(matches!(result, Err(XllError::Input { .. })));
    }

    #[test]
    fn finish_rejects_an_incomplete_argument_stream() {
        let builder = InputFingerprintBuilder::new(1);
        assert!(matches!(
            builder.finish(),
            Err(XllError::Input {
                reason: InputError::Malformed("argument count mismatch"),
                ..
            })
        ));
    }

    #[test]
    fn record_rejects_more_arguments_than_the_wrapper_declared() {
        let mut builder = InputFingerprintBuilder::new(1);
        builder
            .with_argument::<Pair, _, _>(|encoder| {
                Pair(1, 2).encode_identity(encoder);
                Ok(Pair(1, 2))
            })
            .unwrap();
        assert!(matches!(
            builder.with_argument::<Pair, _, _>(|encoder| {
                Pair(3, 4).encode_identity(encoder);
                Ok(Pair(3, 4))
            }),
            Err(XllError::Input {
                reason: InputError::Malformed("too many arguments recorded"),
                ..
            })
        ));
    }
}
