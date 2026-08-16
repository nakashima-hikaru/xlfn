use crate::{DomainErrorCode, InputError, XllError, XllResult};

const MAX_INPUT_IDENTITY_BYTES: usize = 16 * 1024 * 1024;
const ARGUMENT_DOMAIN: &[u8] = b"xlfn-input-argument-v3\0";
const ROOT_DOMAIN: &[u8] = b"xlfn-input-fingerprint-v3\0";

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
/// Implementations of [`ExcelInputIdentity`] encode every value that is
/// observable through the Rust parameter type. The trait's associated domain
/// is written by [`InputFingerprintBuilder`], so an implementation cannot
/// accidentally omit its top-level type separator. The encoder keeps
/// argument-local framing separate from the root fingerprint, so independently
/// encoded arguments cannot be confused with one another.
pub struct InputIdentityEncoder<'a> {
    hasher: &'a mut blake3::Hasher,
    bytes: usize,
    error: Option<XllError>,
}

impl<'a> InputIdentityEncoder<'a> {
    pub(crate) fn new(hasher: &'a mut blake3::Hasher) -> Self {
        Self {
            hasher,
            bytes: 0,
            error: None,
        }
    }

    /// Adds a nested stable domain separator for a semantic type.
    pub fn domain(&mut self, domain: &[u8]) {
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
        self.hasher.update(bytes);
    }

    fn finish(self) -> XllResult<[u8; 32]> {
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

/// Defines which parts of a converted Rust argument participate in formula
/// revision identity.
pub trait ExcelInputIdentity {
    /// Stable domain separator for this semantic Rust type.
    const IDENTITY_DOMAIN: &'static [u8];

    /// Encodes the value without writing [`Self::IDENTITY_DOMAIN`]. Containers
    /// use the associated domain to frame their element stream once.
    fn encode_identity(&self, encoder: &mut InputIdentityEncoder<'_>);
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
        root.update(ROOT_DOMAIN);
        root.update(&(expected_arguments as u64).to_le_bytes());
        Self {
            root,
            expected_arguments,
            recorded_arguments: 0,
            bytes: 0,
        }
    }

    pub(crate) fn record<T: ExcelInputIdentity>(&mut self, value: &T) -> XllResult<()> {
        if self.recorded_arguments >= self.expected_arguments {
            return Err(XllError::input(
                "input_identity",
                InputError::Malformed("too many arguments recorded"),
            ));
        }

        let mut hasher = blake3::Hasher::new();
        hasher.update(ARGUMENT_DOMAIN);
        hasher.update(T::IDENTITY_DOMAIN);
        let mut encoder = InputIdentityEncoder::new(&mut hasher);
        value.encode_identity(&mut encoder);
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
        Ok(())
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
    use crate::{Matrix, OptionalExcelValue};

    #[derive(Clone, Copy)]
    struct Pair(u32, u32);

    impl ExcelInputIdentity for Pair {
        const IDENTITY_DOMAIN: &'static [u8] = b"test.pair.v3";

        fn encode_identity(&self, encoder: &mut InputIdentityEncoder<'_>) {
            encoder.u32(self.0);
            encoder.u32(self.1);
        }
    }

    fn fingerprint<T: ExcelInputIdentity>(values: &[T]) -> InputFingerprint {
        let mut builder = InputFingerprintBuilder::new(values.len());
        for value in values {
            builder.record(value).unwrap();
        }
        builder.finish().unwrap()
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
        argument.update(ARGUMENT_DOMAIN);
        argument.update(<Vec<f64> as ExcelInputIdentity>::IDENTITY_DOMAIN);
        let mut encoder = InputIdentityEncoder::new(&mut argument);
        encoder.domain(f64::IDENTITY_DOMAIN);
        encoder.u64(values.len() as u64);
        for value in &values {
            value.encode_identity(&mut encoder);
        }
        let argument_digest = encoder.finish().unwrap();

        let mut root = blake3::Hasher::new();
        root.update(ROOT_DOMAIN);
        root.update(&1_u64.to_le_bytes());
        root.update(&argument_digest);
        let expected = InputFingerprint::from_bytes(*root.finalize().as_bytes());

        assert_eq!(actual, expected);
    }

    #[test]
    fn too_large_identity_is_rejected() {
        let mut builder = InputFingerprintBuilder::new(1);
        let mut hasher = blake3::Hasher::new();
        hasher.update(ARGUMENT_DOMAIN);
        hasher.update(Pair::IDENTITY_DOMAIN);
        let mut encoder = InputIdentityEncoder::new(&mut hasher);
        encoder.bytes(&vec![0_u8; MAX_INPUT_IDENTITY_BYTES]);
        assert!(matches!(encoder.finish(), Err(XllError::Input { .. })));
        let _ = builder.record(&Pair(1, 2));
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
        builder.record(&Pair(1, 2)).unwrap();
        assert!(matches!(
            builder.record(&Pair(3, 4)),
            Err(XllError::Input {
                reason: InputError::Malformed("too many arguments recorded"),
                ..
            })
        ));
    }
}
