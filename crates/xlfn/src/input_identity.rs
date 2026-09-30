use crate::error::InputError;
use crate::{XllError, XllResult};

const INLINE_ARGUMENT_BYTES: usize = 128;
const HASH_BUFFER_BYTES: usize = 4_096;
const INPUT_FINGERPRINT_DOMAIN: &[u8] = b"xlfn-input-v3\0";

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
enum ArgumentSink {
    Inline {
        bytes: [u8; INLINE_ARGUMENT_BYTES],
        len: usize,
    },
    Hashed(Box<HashedArgument>),
}

struct HashedArgument {
    hasher: blake3::Hasher,
    buffer: [u8; HASH_BUFFER_BYTES],
    buffered: usize,
}

#[cfg(test)]
enum ArgumentIdentity {
    Inline { len: usize },
    Hashed,
}

pub struct InputIdentityEncoder {
    sink: ArgumentSink,
    spare: Option<Box<HashedArgument>>,
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
            spare: None,
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

    /// The same UTF-8 framing as `string`, without an
    /// owned string. The unit-wise length plan avoids decoding surrogate
    /// pairs twice; strict decoding below rejects malformed text before the
    /// argument fingerprint can be completed.
    pub(crate) fn semantic_utf16(&mut self, units: &[u16]) -> XllResult<()> {
        let length = crate::utf16::Utf16Decoder::new(units).utf8_len();
        self.u64(length as u64);
        let mut buffer = [0_u8; 256];
        if length == units.len() {
            for chunk in units.chunks(buffer.len()) {
                for (byte, &unit) in buffer.iter_mut().zip(chunk) {
                    *byte = unit as u8;
                }
                self.write(&buffer[..chunk.len()]);
            }
            return Ok(());
        }
        let mut used = 0;
        for ch in char::decode_utf16(units.iter().copied()) {
            let ch = match ch {
                Ok(ch) => ch,
                Err(_) => {
                    let error = XllError::input(self.argument, InputError::InvalidUtf16);
                    self.fail(error.clone());
                    return Err(error);
                }
            };
            if buffer.len() - used < 4 {
                self.write(&buffer[..used]);
                used = 0;
            }
            used += ch.encode_utf8(&mut buffer[used..]).len();
        }
        self.write(&buffer[..used]);
        Ok(())
    }

    /// Encodes raw Excel text without interpreting surrogate pairs. Borrowed
    /// raw views can observe invalid UTF-16 units, so preserve every unit and
    /// its length. UTF-8 semantic strings use the separate `string` encoding.
    pub(crate) fn utf16(&mut self, units: &[u16]) {
        self.u64(units.len() as u64);
        let mut bytes = [0_u8; 256];
        for chunk in units.chunks(bytes.len() / 2) {
            let (pairs, _) = bytes.as_chunks_mut::<2>();
            for (&unit, pair) in chunk.iter().zip(pairs.iter_mut()) {
                pair.copy_from_slice(&unit.to_le_bytes());
            }
            self.write(&bytes[..chunk.len() * 2]);
        }
    }

    /// Adds a boolean value.
    pub fn bool(&mut self, value: bool) {
        self.write(&[u8::from(value)]);
    }

    /// Adds an `f64` using its converted Rust bit pattern.
    #[inline]
    pub fn f64(&mut self, value: f64) {
        self.u64(value.to_bits());
    }

    /// Appends converted numeric cells with the same framing as repeated
    /// `f64` calls. Selects the hashed sink once for the remaining sequence;
    /// validation stays with the iterator and completes before lookup.
    pub(crate) fn f64_sequence(
        &mut self,
        mut values: impl Iterator<Item = XllResult<f64>>,
    ) -> XllResult<()> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        while let Some(value) = values.next() {
            let value = match value {
                Ok(value) => value,
                Err(error) => {
                    self.fail(error.clone());
                    return Err(error);
                }
            };
            let bytes = value.to_bits().to_le_bytes();
            if let ArgumentSink::Inline { bytes: buffer, len } = &mut self.sink {
                if *len + bytes.len() <= INLINE_ARGUMENT_BYTES {
                    buffer[*len..*len + bytes.len()].copy_from_slice(&bytes);
                    *len += bytes.len();
                    continue;
                }
                self.promote_to_hashed();
            }
            let ArgumentSink::Hashed(state) = &mut self.sink else {
                unreachable!("numeric sequence promotes before streaming");
            };
            Self::write_hashed(
                &mut state.hasher,
                &mut state.buffer,
                &mut state.buffered,
                &bytes,
            );
            let result: XllResult<()> = values.try_for_each(|value| {
                let bytes = value?.to_bits().to_le_bytes();
                Self::write_hashed(
                    &mut state.hasher,
                    &mut state.buffer,
                    &mut state.buffered,
                    &bytes,
                );
                Ok(())
            });
            if let Err(error) = &result {
                self.fail(error.clone());
            }
            return result;
        }
        Ok(())
    }

    /// Adds a little-endian `u32`.
    pub fn u32(&mut self, value: u32) {
        self.write(&value.to_le_bytes());
    }

    /// Adds a little-endian `u64`.
    #[inline]
    pub fn u64(&mut self, value: u64) {
        self.write(&value.to_le_bytes());
    }

    /// Adds a little-endian signed `i64`.
    pub fn i64(&mut self, value: i64) {
        self.write(&value.to_le_bytes());
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        if self.error.is_some() {
            return;
        }

        let needs_promotion = match &self.sink {
            ArgumentSink::Inline { len, .. } => len
                .checked_add(bytes.len())
                .is_none_or(|total| total > INLINE_ARGUMENT_BYTES),
            ArgumentSink::Hashed(_) => false,
        };
        if needs_promotion {
            self.promote_to_hashed();
        }

        match &mut self.sink {
            ArgumentSink::Inline { bytes: buffer, len } => {
                buffer[*len..*len + bytes.len()].copy_from_slice(bytes);
                *len += bytes.len();
            }
            ArgumentSink::Hashed(state) => Self::write_hashed(
                &mut state.hasher,
                &mut state.buffer,
                &mut state.buffered,
                bytes,
            ),
        }
    }

    // Promotion happens once per large argument, never for a scalar. Keeping
    // allocation construction out of write's inline frame avoids imposing a
    // multi-kilobyte stack frame on every short worksheet argument.
    #[cold]
    #[inline(never)]
    fn promote_to_hashed(&mut self) {
        let previous = std::mem::replace(
            &mut self.sink,
            ArgumentSink::Hashed(if let Some(mut state) = self.spare.take() {
                state.hasher.reset();
                state
            } else {
                Box::new(HashedArgument {
                    hasher: blake3::Hasher::new(),
                    buffer: [0; HASH_BUFFER_BYTES],
                    buffered: 0,
                })
            }),
        );
        let ArgumentSink::Inline { bytes, len } = previous else {
            unreachable!("identity encoder promotes only from inline mode");
        };
        let ArgumentSink::Hashed(state) = &mut self.sink else {
            unreachable!("identity encoder promotion must create a hashed sink");
        };
        state.buffer[..len].copy_from_slice(&bytes[..len]);
        state.buffered = len;
    }

    #[inline]
    fn write_hashed(
        hasher: &mut blake3::Hasher,
        buffer: &mut [u8; HASH_BUFFER_BYTES],
        buffered: &mut usize,
        bytes: &[u8],
    ) {
        if bytes.len() >= HASH_BUFFER_BYTES {
            Self::flush_hashed(hasher, buffer, buffered);
            hasher.update(bytes);
            return;
        }
        if *buffered + bytes.len() > HASH_BUFFER_BYTES {
            Self::flush_hashed(hasher, buffer, buffered);
        }
        buffer[*buffered..*buffered + bytes.len()].copy_from_slice(bytes);
        *buffered += bytes.len();
    }

    fn flush_hashed(
        hasher: &mut blake3::Hasher,
        buffer: &[u8; HASH_BUFFER_BYTES],
        buffered: &mut usize,
    ) {
        if *buffered == 0 {
            return;
        }
        hasher.update(&buffer[..*buffered]);
        *buffered = 0;
    }

    fn finish_into(&mut self, root: &mut blake3::Hasher) -> XllResult<()> {
        match &self.error {
            Some(error) => Err(error.clone()),
            None => match &mut self.sink {
                ArgumentSink::Inline { bytes, len } => {
                    root.update(&[ArgumentEncoding::Inline as u8]);
                    root.update(&(*len as u64).to_le_bytes());
                    root.update(&bytes[..*len]);
                    Ok(())
                }
                ArgumentSink::Hashed(state) => {
                    Self::flush_hashed(&mut state.hasher, &state.buffer, &mut state.buffered);
                    root.update(&[ArgumentEncoding::Hashed as u8]);
                    root.update(state.hasher.finalize().as_bytes());
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
                ArgumentSink::Hashed(mut state) => {
                    Self::flush_hashed(&mut state.hasher, &state.buffer, &mut state.buffered);
                    let _ = state.hasher.finalize();
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
    // One owned workspace reused between arguments, released with the call.
    scratch: Option<Box<HashedArgument>>,
}

impl InputFingerprintBuilder {
    #[inline]
    pub(crate) fn new(expected_arguments: usize) -> Self {
        let mut root = blake3::Hasher::new();
        root.update(INPUT_FINGERPRINT_DOMAIN);
        root.update(&(expected_arguments as u64).to_le_bytes());
        Self {
            root,
            expected_arguments,
            next_argument: 0,
            scratch: None,
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
        encoder.spare = self.scratch.take();
        let result = encode(&mut encoder).and_then(|value| {
            encoder.finish_into(&mut self.root)?;
            Ok(value)
        });
        self.scratch = match encoder.sink {
            ArgumentSink::Hashed(state) => Some(state),
            ArgumentSink::Inline { .. } => encoder.spare,
        };
        let value = result?;
        self.next_argument += 1;
        Ok(value)
    }

    #[inline]
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

    proptest::proptest! {
        #[test]
        fn semantic_utf16_matches_owned_unicode(text in ".{0,512}") {
            let units = text.encode_utf16().collect::<Vec<_>>();
            let mut actual = InputIdentityEncoder::new("text");
            actual.semantic_utf16(&units).unwrap();
            let mut expected = InputIdentityEncoder::new("text");
            expected.string(&text);
            let mut a = blake3::Hasher::new();
            let mut b = blake3::Hasher::new();
            actual.finish_into(&mut a).unwrap();
            expected.finish_into(&mut b).unwrap();
            proptest::prop_assert_eq!(a.finalize(), b.finalize());
        }
    }

    #[test]
    fn numeric_sequences_preserve_framing_bits_and_hash_buffer_boundaries() {
        let prefixes: &[usize] = if cfg!(miri) {
            &[0, 127]
        } else {
            &[0, 1, 16, 127, 128, 129]
        };
        let counts: &[usize] = if cfg!(miri) {
            &[15, 64, 513]
        } else {
            &[0, 1, 15, 16, 17, 63, 64, 65, 511, 512, 513, 1025]
        };
        for &prefix in prefixes {
            for &count in counts {
                let mut actual = InputIdentityEncoder::new("values");
                let mut expected = InputIdentityEncoder::new("values");
                actual.write(&vec![0xa5; prefix]);
                expected.write(&vec![0xa5; prefix]);
                let values = (0..count).map(|index| match index % 3 {
                    0 => -0.0,
                    1 => -(index as f64),
                    _ => index as f64 / 7.0,
                });
                for value in values.clone() {
                    expected.f64(value);
                }
                actual.f64_sequence(values.map(Ok)).unwrap();
                let mut a = blake3::Hasher::new();
                let mut b = blake3::Hasher::new();
                actual.finish_into(&mut a).unwrap();
                expected.finish_into(&mut b).unwrap();
                assert_eq!(a.finalize(), b.finalize(), "prefix={prefix}, count={count}");
            }
        }
    }

    #[test]
    fn numeric_sequence_validation_errors_poison_partial_identity() {
        for prefix in [0, 1, 63, 64, 65, 512] {
            let mut encoder = InputIdentityEncoder::new("values");
            let values = (0..prefix)
                .map(|index| Ok(index as f64))
                .chain(std::iter::once(Err(XllError::input(
                    "<array cell>",
                    InputError::NonFinite,
                ))));
            assert!(encoder.f64_sequence(values).is_err());
            encoder.f64(42.0);
            assert!(matches!(
                encoder.finish_into(&mut blake3::Hasher::new()),
                Err(XllError::Input {
                    argument: "values",
                    reason: InputError::NonFinite,
                })
            ));
        }
    }

    #[test]
    fn malformed_utf16_cannot_finalize_a_partial_identity() {
        for prefix in [0, 1, 127, 128, 255, 256, 4096] {
            for invalid in [0xd800, 0xdc00] {
                let mut units = vec![0x61; prefix];
                units.push(invalid);
                let mut encoder = InputIdentityEncoder::new("text");
                assert!(encoder.semantic_utf16(&units).is_err());
                // Even a caller that ignores the conversion error cannot
                // publish the valid prefix as a successful identity.
                assert!(encoder.finish_into(&mut blake3::Hasher::new()).is_err());
            }
        }
    }

    #[test]
    fn reused_hash_workspace_preserves_argument_framing_and_nested_encoders() {
        let mut builder = InputFingerprintBuilder::new(6);
        let mut reference = blake3::Hasher::new();
        reference.update(INPUT_FINGERPRINT_DOMAIN);
        reference.update(&6_u64.to_le_bytes());
        for (index, length) in [8192, 1, 129, 128, 4095, 4097].into_iter().enumerate() {
            let bytes = vec![index as u8; length];
            builder
                .with_argument(index, "arg", |encoder| {
                    for chunk in bytes.chunks(17) {
                        encoder.write(chunk);
                    }
                    let mut nested = InputFingerprintBuilder::new(1);
                    nested.with_argument(0, "nested", |encoder| {
                        encoder.bytes(&vec![0xfe; 2048]);
                        Ok(())
                    })?;
                    nested.finish()?;
                    Ok(())
                })
                .unwrap();
            reference.update(&[0xa0]);
            reference.update(&(index as u64).to_le_bytes());
            reference.update(&3_u64.to_le_bytes());
            reference.update(b"arg");
            if length <= INLINE_ARGUMENT_BYTES {
                reference.update(&[ArgumentEncoding::Inline as u8]);
                reference.update(&(length as u64).to_le_bytes());
                reference.update(&bytes);
            } else {
                reference.update(&[ArgumentEncoding::Hashed as u8]);
                reference.update(blake3::hash(&bytes).as_bytes());
            }
        }
        assert_eq!(
            builder.finish().unwrap().as_bytes(),
            reference.finalize().as_bytes()
        );
    }

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
    fn raw_utf16_batches_match_length_delimited_little_endian_units() {
        for length in [0, 1, 59, 60, 61, 63, 64, 127, 128, 129, 4_096] {
            let units = (0..length)
                .map(|index| (index * 997) as u16)
                .collect::<Vec<_>>();
            let mut batched = InputIdentityEncoder::new("text");
            batched.utf16(&units);
            let mut reference = InputIdentityEncoder::new("text");
            reference.u64(length as u64);
            let bytes = units
                .iter()
                .flat_map(|unit| unit.to_le_bytes())
                .collect::<Vec<_>>();
            reference.write(&bytes);
            let mut actual = blake3::Hasher::new();
            batched.finish_into(&mut actual).unwrap();
            let mut expected = blake3::Hasher::new();
            reference.finish_into(&mut expected).unwrap();
            assert_eq!(actual.finalize(), expected.finalize(), "length={length}");
        }
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
    fn hashed_identity_is_independent_of_write_boundaries() {
        for length in [0, 127, 128, 129, 4_095, 4_096, 4_097, 12_289] {
            let payload = (0..length).map(|index| index as u8).collect::<Vec<_>>();
            let mut expected = blake3::Hasher::new();
            if length <= INLINE_ARGUMENT_BYTES {
                expected.update(&[ArgumentEncoding::Inline as u8]);
                expected.update(&(length as u64).to_le_bytes());
                expected.update(&payload);
            } else {
                expected.update(&[ArgumentEncoding::Hashed as u8]);
                expected.update(blake3::hash(&payload).as_bytes());
            }
            let expected = expected.finalize();
            for chunk in [1, 7, 8, 127, 128, 129, 4_095, 4_096, 4_097, 16_384] {
                let mut encoder = InputIdentityEncoder::new("value");
                for part in payload.chunks(chunk) {
                    encoder.write(part);
                }
                let mut actual = blake3::Hasher::new();
                encoder.finish_into(&mut actual).unwrap();
                assert_eq!(
                    actual.finalize(),
                    expected,
                    "length={length}, chunk={chunk}"
                );
            }
        }
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
