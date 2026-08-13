use crate::{InputError, XlValueRef, XllError, XllResult};
use smallvec::SmallVec;
use xlfn_sys::{
    XLOPER12, XLTYPE_BOOL, XLTYPE_ERR, XLTYPE_INT, XLTYPE_MISSING, XLTYPE_MULTI, XLTYPE_NIL,
    XLTYPE_NUM, XLTYPE_STR,
};

const MAX_FINGERPRINT_BYTES: usize = 16 * 1024 * 1024;
const HASH_BUFFER_BYTES: usize = 256;
const BUFFERED_STRING_THRESHOLD_UNITS: usize = 16;
const DOMAIN: &[u8] = b"xlfn-formula-fingerprint-v1\0";

const TAG_NUMBER: u8 = 1;
const TAG_BOOLEAN: u8 = 2;
const TAG_INTEGER: u8 = 3;
const TAG_STRING: u8 = 4;
const TAG_ERROR: u8 = 5;
const TAG_MISSING: u8 = 6;
const TAG_BLANK: u8 = 7;
const TAG_ARRAY: u8 = 8;

pub(crate) unsafe fn fingerprint(arguments: &[*mut XLOPER12]) -> XllResult<[u8; 32]> {
    if arguments_require_buffer(arguments)? {
        let mut encoder = BufferedFingerprintEncoder::new();
        encode_arguments(&mut encoder, arguments)?;
        Ok(encoder.finish())
    } else {
        let mut encoder = FingerprintEncoder::new();
        encode_arguments(&mut encoder, arguments)?;
        Ok(encoder.finish())
    }
}

fn encode_arguments<S: FingerprintSink>(
    encoder: &mut S,
    arguments: &[*mut XLOPER12],
) -> XllResult<()> {
    encoder.write(DOMAIN)?;
    encoder.write_u64(arguments.len() as u64)?;
    for argument in arguments {
        // SAFETY: ReturnContext::for_call requires each raw argument to remain
        // live for the context lifetime.
        let value = unsafe { XlValueRef::from_raw(*argument) }?;
        encoder.write_value(value, false)?;
    }
    Ok(())
}

fn arguments_require_buffer(arguments: &[*mut XLOPER12]) -> XllResult<bool> {
    if arguments.len() > 4 {
        return Ok(true);
    }

    for argument in arguments {
        // SAFETY: fingerprint's caller supplies live XLOPER12 arguments for
        // the duration of the call.
        let value = unsafe { XlValueRef::from_raw(*argument) }?;
        match value.base_type() {
            XLTYPE_MULTI => return Ok(true),
            XLTYPE_STR
                if value.utf16("formula_fingerprint")?.len() >= BUFFERED_STRING_THRESHOLD_UNITS =>
            {
                return Ok(true);
            }
            _ => {}
        }
    }

    Ok(false)
}

trait FingerprintSink: Sized {
    fn write(&mut self, bytes: &[u8]) -> XllResult<()>;
    fn finish(self) -> [u8; 32];

    fn write_tag(&mut self, tag: u8) -> XllResult<()> {
        self.write(&[tag])
    }

    fn write_u16(&mut self, value: u16) -> XllResult<()> {
        self.write(&value.to_le_bytes())
    }

    fn write_u32(&mut self, value: u32) -> XllResult<()> {
        self.write(&value.to_le_bytes())
    }

    fn write_u64(&mut self, value: u64) -> XllResult<()> {
        self.write(&value.to_le_bytes())
    }

    fn write_value(&mut self, value: XlValueRef<'_>, nested: bool) -> XllResult<()> {
        match value.base_type() {
            XLTYPE_NUM => {
                self.write_tag(TAG_NUMBER)?;
                // SAFETY: XLTYPE_NUM selects the number union member.
                let number = unsafe { value.raw().value.number };
                let bits = if number == 0.0 {
                    0.0_f64.to_bits()
                } else if number.is_nan() {
                    0x7ff8_0000_0000_0000
                } else {
                    number.to_bits()
                };
                self.write_u64(bits)
            }
            XLTYPE_BOOL => {
                self.write_tag(TAG_BOOLEAN)?;
                // Preserve the raw representation so non-canonical values cannot
                // collide with a valid Excel boolean in formula identity.
                // SAFETY: XLTYPE_BOOL selects the boolean union member.
                let boolean = unsafe { value.raw().value.boolean };
                self.write_u32(boolean as u32)
            }
            XLTYPE_INT => {
                self.write_tag(TAG_INTEGER)?;
                // SAFETY: XLTYPE_INT selects the integer union member.
                let integer = unsafe { value.raw().value.integer };
                self.write_u32(integer as u32)
            }
            XLTYPE_STR => {
                self.write_tag(TAG_STRING)?;
                let text = value.utf16("formula_fingerprint")?;
                self.write_u64(text.len() as u64)?;
                for unit in text {
                    self.write_u16(*unit)?;
                }
                Ok(())
            }
            XLTYPE_ERR => {
                self.write_tag(TAG_ERROR)?;
                // SAFETY: XLTYPE_ERR selects the error union member.
                let error = unsafe { value.raw().value.error };
                self.write_u32(error as u32)
            }
            XLTYPE_MISSING => self.write_tag(TAG_MISSING),
            XLTYPE_NIL => self.write_tag(TAG_BLANK),
            XLTYPE_MULTI if !nested => {
                self.write_tag(TAG_ARRAY)?;
                let array = value.array("formula_fingerprint")?;
                self.write_u64(array.rows as u64)?;
                self.write_u64(array.columns as u64)?;
                let elements = (array.rows as usize) * (array.columns as usize);

                const BATCH_CAPACITY: usize = 64;
                let mut chunk_buffer = [0u8; BATCH_CAPACITY * 9];
                let mut chunk_len = 0;

                for index in 0..elements {
                    // SAFETY: XlValueRef::array validated the contiguous
                    // element range and the index is within its dimensions.
                    let elem_ptr = unsafe { array.values.add(index) };
                    // SAFETY: `elem_ptr` points to a readable XLOPER12 in the validated array.
                    let is_num = unsafe { (*elem_ptr).base_type() == XLTYPE_NUM };
                    if is_num {
                        // SAFETY: XLTYPE_NUM selects the number union member.
                        let number = unsafe { (*elem_ptr).value.number };
                        let bits = if number == 0.0 {
                            0.0_f64.to_bits()
                        } else if number.is_nan() {
                            0x7ff8_0000_0000_0000
                        } else {
                            number.to_bits()
                        };
                        chunk_buffer[chunk_len] = TAG_NUMBER;
                        chunk_buffer[chunk_len + 1..chunk_len + 9]
                            .copy_from_slice(&bits.to_le_bytes());
                        chunk_len += 9;

                        if chunk_len == chunk_buffer.len() {
                            self.write(&chunk_buffer)?;
                            chunk_len = 0;
                        }
                    } else {
                        if chunk_len > 0 {
                            self.write(&chunk_buffer[..chunk_len])?;
                            chunk_len = 0;
                        }
                        // SAFETY: `elem_ptr` is a readable XLOPER12 within the validated array.
                        let element = unsafe { XlValueRef::from_raw(elem_ptr) }?;
                        self.write_value(element, true)?;
                    }
                }

                if chunk_len > 0 {
                    self.write(&chunk_buffer[..chunk_len])?;
                }
                Ok(())
            }
            XLTYPE_MULTI => Err(XllError::input(
                "formula_fingerprint",
                InputError::Malformed("nested arrays are not supported"),
            )),
            _ => Err(XllError::input(
                "formula_fingerprint",
                InputError::WrongType {
                    expected: "worksheet value",
                    actual: value.base_type(),
                },
            )),
        }
    }
}

struct FingerprintEncoder {
    hasher: blake3::Hasher,
    bytes: usize,
}

struct BufferedFingerprintEncoder {
    hasher: blake3::Hasher,
    bytes: usize,
    buffered: usize,
    buffer: SmallVec<[u8; HASH_BUFFER_BYTES]>,
}

impl FingerprintEncoder {
    fn new() -> Self {
        Self {
            hasher: blake3::Hasher::new(),
            bytes: 0,
        }
    }
}

impl FingerprintSink for FingerprintEncoder {
    fn write(&mut self, bytes: &[u8]) -> XllResult<()> {
        let actual = self
            .bytes
            .checked_add(bytes.len())
            .ok_or(XllError::Domain {
                code: crate::DomainErrorCode::Overflow,
            })?;
        if actual > MAX_FINGERPRINT_BYTES {
            return Err(XllError::input(
                "formula_fingerprint",
                InputError::TooLarge {
                    limit: MAX_FINGERPRINT_BYTES,
                    actual,
                },
            ));
        }
        self.bytes = actual;
        self.hasher.update(bytes);
        Ok(())
    }

    fn finish(self) -> [u8; 32] {
        *self.hasher.finalize().as_bytes()
    }
}

impl BufferedFingerprintEncoder {
    fn new() -> Self {
        Self {
            hasher: blake3::Hasher::new(),
            bytes: 0,
            buffered: 0,
            buffer: SmallVec::new(),
        }
    }

    fn flush(&mut self) {
        if self.buffered == 0 {
            return;
        }

        self.hasher.update(&self.buffer[..self.buffered]);
        self.buffered = 0;
    }

    fn write_buffered(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            // Avoid an unnecessary copy when both the staging buffer is empty
            // and the caller already supplied at least one full-sized chunk.
            if self.buffered == 0 && bytes.len() >= HASH_BUFFER_BYTES {
                let direct = bytes.len() / HASH_BUFFER_BYTES * HASH_BUFFER_BYTES;
                self.hasher.update(&bytes[..direct]);
                bytes = &bytes[direct..];
                continue;
            }

            let available = HASH_BUFFER_BYTES - self.buffered;
            let copied = available.min(bytes.len());

            if self.buffer.is_empty() {
                self.buffer.resize(HASH_BUFFER_BYTES, 0);
            }

            self.buffer[self.buffered..self.buffered + copied].copy_from_slice(&bytes[..copied]);
            self.buffered += copied;
            bytes = &bytes[copied..];

            if self.buffered == HASH_BUFFER_BYTES {
                self.flush();
            }
        }
    }
}

impl FingerprintSink for BufferedFingerprintEncoder {
    fn write(&mut self, bytes: &[u8]) -> XllResult<()> {
        let actual = self
            .bytes
            .checked_add(bytes.len())
            .ok_or(XllError::Domain {
                code: crate::DomainErrorCode::Overflow,
            })?;
        if actual > MAX_FINGERPRINT_BYTES {
            return Err(XllError::input(
                "formula_fingerprint",
                InputError::TooLarge {
                    limit: MAX_FINGERPRINT_BYTES,
                    actual,
                },
            ));
        }
        self.bytes = actual;
        self.write_buffered(bytes);
        Ok(())
    }
    fn finish(self) -> [u8; 32] {
        let mut encoder = self;
        encoder.flush();
        *encoder.hasher.finalize().as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xlfn_sys::{XLOPER12Array, XLOPER12Value};

    fn digest(value: &mut XLOPER12) -> [u8; 32] {
        // SAFETY: the stack-local XLOPER12 remains live for this call.
        unsafe { fingerprint(&[value]) }.unwrap()
    }

    #[test]
    fn numbers_normalize_negative_zero_but_keep_type_tags() {
        let mut positive_zero = XLOPER12::number(0.0);
        let mut negative_zero = XLOPER12::number(-0.0);
        let mut integer_zero = XLOPER12::integer(0);
        assert_eq!(digest(&mut positive_zero), digest(&mut negative_zero));
        assert_ne!(digest(&mut positive_zero), digest(&mut integer_zero));
    }

    #[test]
    fn missing_blank_and_error_are_distinct() {
        let mut missing = XLOPER12::missing();
        let mut blank = XLOPER12::nil();
        let mut error = XLOPER12::error(xlfn_sys::XLERR_NA);
        assert_ne!(digest(&mut missing), digest(&mut blank));
        assert_ne!(digest(&mut missing), digest(&mut error));
        assert_ne!(digest(&mut blank), digest(&mut error));
    }

    #[test]
    fn strings_hash_utf16_code_units_and_arrays_hash_shape() {
        let mut text = vec![2_u16, b'a' as u16, 0x3042];
        let mut string = XLOPER12 {
            value: XLOPER12Value {
                string: text.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };
        let string_digest = digest(&mut string);
        text[2] = 0x3044;
        assert_ne!(string_digest, digest(&mut string));

        let mut cells = [XLOPER12::number(1.0), XLOPER12::number(2.0)];
        let mut row = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: cells.as_mut_ptr(),
                    rows: 1,
                    columns: 2,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        let mut column = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: cells.as_mut_ptr(),
                    rows: 2,
                    columns: 1,
                },
            },
            xltype: XLTYPE_MULTI,
        };
        assert_ne!(digest(&mut row), digest(&mut column));
    }

    #[test]
    fn complete_handle_tokens_are_fingerprinted() {
        let first_token = "xllh:3:0000000000000001:00000000:0000000000000001:aaaaaaaa";
        let second_token = "xllh:3:0000000000000001:00000000:0000000000000002:aaaaaaaa";
        let mut first_text = Vec::with_capacity(first_token.encode_utf16().count() + 1);
        first_text.push(first_token.encode_utf16().count() as u16);
        first_text.extend(first_token.encode_utf16());
        let mut second_text = Vec::with_capacity(second_token.encode_utf16().count() + 1);
        second_text.push(second_token.encode_utf16().count() as u16);
        second_text.extend(second_token.encode_utf16());
        let mut first = XLOPER12 {
            value: XLOPER12Value {
                string: first_text.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };
        let mut second = XLOPER12 {
            value: XLOPER12Value {
                string: second_text.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };
        assert_ne!(digest(&mut first), digest(&mut second));
    }

    #[test]
    fn nan_payloads_are_normalized_to_one_quiet_nan() {
        let mut first = XLOPER12::number(f64::from_bits(0x7ff8_0000_0000_0001));
        let mut second = XLOPER12::number(f64::from_bits(0xfff0_0000_0000_0002));
        assert_eq!(digest(&mut first), digest(&mut second));
    }

    #[test]
    fn fingerprint_budget_is_enforced_incrementally() {
        let mut encoder = FingerprintEncoder::new();
        encoder.bytes = MAX_FINGERPRINT_BYTES;
        assert!(matches!(
            encoder.write(&[1]),
            Err(XllError::Input {
                reason: InputError::TooLarge { .. },
                ..
            })
        ));
    }

    #[test]
    fn buffered_encoder_matches_direct_blake3_stream() {
        let parts: [&[u8]; 5] = [
            b"small",
            &[0; 3],
            &[1; HASH_BUFFER_BYTES - 4],
            &[2; HASH_BUFFER_BYTES + 17],
            b"tail",
        ];

        let mut encoder = BufferedFingerprintEncoder::new();
        let mut direct = blake3::Hasher::new();

        for part in parts {
            encoder.write(part).unwrap();
            direct.update(part);
        }

        assert_eq!(encoder.finish(), *direct.finalize().as_bytes());
    }

    #[test]
    fn batched_numeric_array_matches_unbatched_byte_stream() {
        let mut cells = vec![
            XLOPER12::number(0.0),
            XLOPER12::number(-0.0),
            XLOPER12::number(f64::from_bits(0x7ff8_0000_0000_0001)),
            XLOPER12::number(42.5),
            XLOPER12::boolean(true),
            XLOPER12::error(xlfn_sys::XLERR_VALUE),
            XLOPER12::missing(),
            XLOPER12::nil(),
        ];
        // Extend with 200 numbers to cross the 64-element batch boundary multiple times
        for i in 0..200 {
            cells.push(XLOPER12::number(i as f64));
        }

        let mut array = XLOPER12 {
            value: XLOPER12Value {
                array: XLOPER12Array {
                    values: cells.as_mut_ptr(),
                    rows: cells.len() as i32,
                    columns: 1,
                },
            },
            xltype: XLTYPE_MULTI,
        };

        // Compute actual digest via fingerprint()
        let actual_digest = digest(&mut array);

        // Manually construct expected bytes without batching
        let mut expected_encoder = BufferedFingerprintEncoder::new();
        expected_encoder.write(DOMAIN).unwrap();
        expected_encoder.write_u64(1).unwrap(); // 1 argument
        expected_encoder.write_tag(TAG_ARRAY).unwrap();
        expected_encoder.write_u64(cells.len() as u64).unwrap(); // rows
        expected_encoder.write_u64(1).unwrap(); // columns

        for cell in &cells {
            // SAFETY: `cell` is a readable stack-allocated XLOPER12.
            let value = unsafe { XlValueRef::from_raw(cell as *const _ as *mut _) }.unwrap();
            expected_encoder.write_value(value, true).unwrap();
        }

        assert_eq!(actual_digest, expected_encoder.finish());
    }
}
