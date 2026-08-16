use crate::{DomainErrorCode, ExcelParameter, InputError, XllError, XllResult};

const MAX_INPUT_IDENTITY_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const ARGUMENT_DOMAIN: &[u8] = b"xlfn-input-argument-v4\0";
pub(crate) const ROOT_DOMAIN: &[u8] = b"xlfn-input-fingerprint-v4\0";
const DEFAULT_WRITE_BUFFER: usize = 64;
pub(crate) const ROOT_PREFIX_BYTES: usize = 8 + ROOT_DOMAIN.len() + 8;

/// The fixed-size semantic identity of one converted Excel argument list.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InputFingerprint([u8; 32]);

impl InputFingerprint {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Encodes the semantic identity of one converted Excel argument.
///
/// Implementations of [`ExcelParameter`] encode every value that is observable
/// through the Rust parameter type. The trait's associated domain is written
/// by [`InputFingerprintBuilder`], so an implementation cannot accidentally
/// omit its top-level type separator. The encoder keeps argument-local framing
/// separate from the root fingerprint, so independently encoded arguments
/// cannot be confused with one another.
pub struct InputIdentityEncoder<'a, const WRITE_BUFFER: usize = DEFAULT_WRITE_BUFFER> {
    hasher: &'a mut blake3::Hasher,
    buffer: [u8; WRITE_BUFFER],
    buffered: usize,
    bytes: usize,
    error: Option<XllError>,
}

impl<'a, const WRITE_BUFFER: usize> InputIdentityEncoder<'a, WRITE_BUFFER> {
    pub(crate) fn new(hasher: &'a mut blake3::Hasher) -> Self {
        Self {
            hasher,
            buffer: [0; WRITE_BUFFER],
            buffered: 0,
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

        if WRITE_BUFFER == 0 {
            self.hasher.update(bytes);
            return;
        }

        if bytes.len() >= WRITE_BUFFER {
            self.flush();
            self.hasher.update(bytes);
            return;
        }

        if self.buffered + bytes.len() > WRITE_BUFFER {
            self.flush();
        }
        self.buffer[self.buffered..self.buffered + bytes.len()].copy_from_slice(bytes);
        self.buffered += bytes.len();
    }

    fn flush(&mut self) {
        if self.buffered == 0 {
            return;
        }

        self.hasher.update(&self.buffer[..self.buffered]);
        self.buffered = 0;
    }

    pub(crate) fn finish(mut self) -> XllResult<[u8; 32]> {
        if self.error.is_none() {
            self.flush();
        }
        match self.error {
            Some(error) => Err(error),
            None => Ok(*self.hasher.finalize().as_bytes()),
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
        F: FnOnce(&mut InputIdentityEncoder<'_>) -> XllResult<R>,
    {
        if self.recorded_arguments >= self.expected_arguments {
            return Err(XllError::input(
                "input_identity",
                InputError::Malformed("too many arguments recorded"),
            ));
        }

        let mut hasher = blake3::Hasher::new();
        let mut encoder = InputIdentityEncoder::<DEFAULT_WRITE_BUFFER>::new(&mut hasher);
        encoder.domain(ARGUMENT_DOMAIN);
        encoder.domain(T::IDENTITY_DOMAIN);
        let value = encode(&mut encoder)?;
        let encoded_bytes = encoder.bytes_written();
        let digest = encoder.finish()?;
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
        self.root.update(&digest);
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
        const IDENTITY_DOMAIN: &'static [u8] = b"test.pair.v4";

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

        fn encode_identity(&self, encoder: &mut InputIdentityEncoder<'_>) {
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

    fn reference_fingerprint<F>(domain: &[u8], encode: F) -> InputFingerprint
    where
        F: FnOnce(&mut InputIdentityEncoder<'_, 0>),
    {
        let mut argument = blake3::Hasher::new();
        let mut encoder = InputIdentityEncoder::<0>::new(&mut argument);
        encoder.domain(ARGUMENT_DOMAIN);
        encoder.domain(domain);
        encode(&mut encoder);
        let argument_digest = encoder.finish().unwrap();

        let mut root = blake3::Hasher::new();
        root.update(&(ROOT_DOMAIN.len() as u64).to_le_bytes());
        root.update(ROOT_DOMAIN);
        root.update(&1_u64.to_le_bytes());
        root.update(&argument_digest);
        InputFingerprint::from_bytes(*root.finalize().as_bytes())
    }

    #[test]
    fn write_batching_preserves_the_v4_fingerprint_for_builtin_values() {
        let number = 42.0_f64;
        assert_eq!(
            fingerprint(&[number]),
            reference_fingerprint(f64::IDENTITY_DOMAIN, |encoder| encoder.f64(number)),
        );

        let string = String::from("short");
        assert_eq!(
            fingerprint(std::slice::from_ref(&string)),
            reference_fingerprint(String::IDENTITY_DOMAIN, |encoder| encoder.string(&string)),
        );

        let optional = Some(42.0_f64);
        assert_eq!(
            fingerprint(std::slice::from_ref(&optional)),
            reference_fingerprint(<Option<f64>>::IDENTITY_DOMAIN, |encoder| {
                encoder.domain(f64::IDENTITY_DOMAIN);
                encoder.tag(1);
                encoder.f64(number);
            }),
        );

        let optional_excel = OptionalExcelValue::Value(number);
        assert_eq!(
            fingerprint(std::slice::from_ref(&optional_excel)),
            reference_fingerprint(<OptionalExcelValue<f64>>::IDENTITY_DOMAIN, |encoder| {
                encoder.domain(f64::IDENTITY_DOMAIN);
                encoder.tag(2);
                encoder.f64(number);
            }),
        );

        let values = vec![number, 7.0];
        assert_eq!(
            fingerprint(std::slice::from_ref(&values)),
            reference_fingerprint(<Vec<f64>>::IDENTITY_DOMAIN, |encoder| {
                encoder.domain(f64::IDENTITY_DOMAIN);
                encoder.u64(values.len() as u64);
                for value in &values {
                    encoder.f64(*value);
                }
            }),
        );

        let matrix = Matrix::new(1, 2, values.clone()).unwrap();
        assert_eq!(
            fingerprint(std::slice::from_ref(&matrix)),
            reference_fingerprint(<Matrix<f64>>::IDENTITY_DOMAIN, |encoder| {
                encoder.domain(f64::IDENTITY_DOMAIN);
                encoder.u64(matrix.rows() as u64);
                encoder.u64(matrix.columns() as u64);
                for value in matrix.as_slice() {
                    encoder.f64(*value);
                }
            }),
        );

        let owned = OwnedExcelValue::Number(number);
        assert_eq!(
            fingerprint(std::slice::from_ref(&owned)),
            reference_fingerprint(OwnedExcelValue::IDENTITY_DOMAIN, |encoder| {
                encoder.tag(0);
                encoder.f64(number);
            }),
        );
    }

    #[test]
    fn write_batching_preserves_the_v4_fingerprint_for_custom_parameters() {
        let pair = Pair(7, 11);
        assert_eq!(
            fingerprint(std::slice::from_ref(&pair)),
            reference_fingerprint(Pair::IDENTITY_DOMAIN, |encoder| {
                encoder.u32(pair.0);
                encoder.u32(pair.1);
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

        let mut argument = blake3::Hasher::new();
        let mut encoder = InputIdentityEncoder::<DEFAULT_WRITE_BUFFER>::new(&mut argument);
        encoder.domain(ARGUMENT_DOMAIN);
        encoder.domain(<Vec<f64> as ExcelParameter<'_>>::IDENTITY_DOMAIN);
        encoder.domain(f64::IDENTITY_DOMAIN);
        encoder.u64(values.len() as u64);
        for value in &values {
            value.encode_identity(&mut encoder);
        }
        let argument_digest = encoder.finish().unwrap();

        let mut root = blake3::Hasher::new();
        root.update(&(ROOT_DOMAIN.len() as u64).to_le_bytes());
        root.update(ROOT_DOMAIN);
        root.update(&1_u64.to_le_bytes());
        root.update(&argument_digest);
        let expected = InputFingerprint::from_bytes(*root.finalize().as_bytes());

        assert_eq!(actual, expected);
    }

    #[test]
    fn domain_framing_separates_namespace_from_payload() {
        fn digest(domain: &[u8], payload: &[u8]) -> [u8; 32] {
            let mut hasher = blake3::Hasher::new();
            let mut encoder = InputIdentityEncoder::<0>::new(&mut hasher);
            encoder.domain(domain);
            encoder.bytes(payload);
            encoder.finish().unwrap()
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
