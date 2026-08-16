use crate::{DomainErrorCode, InputError, XllError, XllResult};

const MAX_INPUT_IDENTITY_BYTES: usize = 16 * 1024 * 1024;
const ARGUMENT_DOMAIN: &[u8] = b"xlfn-input-argument-v2\0";
const ROOT_DOMAIN: &[u8] = b"xlfn-input-fingerprint-v2\0";

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
/// Implementations of [`ExcelInputIdentity`] should include a stable type
/// domain and then encode every value that is observable through the Rust
/// parameter type. The encoder keeps argument-local framing separate from the
/// root fingerprint, so independently encoded arguments cannot be confused
/// with one another.
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

    /// Adds a stable domain separator for the Rust semantic type.
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
    fn input_identity(&self, encoder: &mut InputIdentityEncoder<'_>);
}

pub(crate) struct InputFingerprintBuilder {
    arguments: Vec<[u8; 32]>,
    bytes: usize,
}

impl InputFingerprintBuilder {
    pub(crate) fn new() -> Self {
        Self {
            arguments: Vec::new(),
            bytes: 0,
        }
    }

    pub(crate) fn record<T: ExcelInputIdentity>(&mut self, value: &T) -> XllResult<()> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(ARGUMENT_DOMAIN);
        let mut encoder = InputIdentityEncoder::new(&mut hasher);
        value.input_identity(&mut encoder);
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
        self.arguments.push(digest);
        Ok(())
    }

    pub(crate) fn finish(self) -> XllResult<InputFingerprint> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(ROOT_DOMAIN);
        let argument_count = u64::try_from(self.arguments.len()).map_err(|_| XllError::Domain {
            code: DomainErrorCode::Overflow,
        })?;
        hasher.update(&argument_count.to_le_bytes());
        for argument in self.arguments {
            hasher.update(&argument);
        }
        Ok(InputFingerprint::from_bytes(*hasher.finalize().as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Matrix, OptionalExcelValue};

    #[derive(Clone, Copy)]
    struct Pair(u32, u32);

    impl ExcelInputIdentity for Pair {
        fn input_identity(&self, encoder: &mut InputIdentityEncoder<'_>) {
            encoder.domain(b"test.pair.v1");
            encoder.u32(self.0);
            encoder.u32(self.1);
        }
    }

    fn fingerprint<T: ExcelInputIdentity>(values: &[T]) -> InputFingerprint {
        let mut builder = InputFingerprintBuilder::new();
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
    fn too_large_identity_is_rejected() {
        let mut builder = InputFingerprintBuilder::new();
        let mut hasher = blake3::Hasher::new();
        hasher.update(ARGUMENT_DOMAIN);
        let mut encoder = InputIdentityEncoder::new(&mut hasher);
        encoder.bytes(&vec![0_u8; MAX_INPUT_IDENTITY_BYTES]);
        assert!(matches!(encoder.finish(), Err(XllError::Input { .. })));
        let _ = builder.record(&Pair(1, 2));
    }
}
