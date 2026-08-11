use super::*;

pub(crate) fn drop_handle_values(
    values: impl IntoIterator<Item = Arc<dyn Any + Send + Sync>>,
    operation: &'static str,
) -> XllResult<()> {
    let mut failure = None;
    for value in values {
        if catch_unwind(AssertUnwindSafe(|| drop(value))).is_err() {
            crate::diagnostics::report_no_unwind(operation, &XllError::Panic);
            if failure.is_none() {
                failure = Some(XllError::Panic);
            }
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub(crate) fn encode_tag(tag: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(32);
    for byte in tag {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

pub(crate) fn decode_tag(encoded: &str) -> Option<[u8; 16]> {
    if encoded.len() != 32 {
        return None;
    }
    let mut tag = [0_u8; 16];
    for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
        tag[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Some(tag)
}

pub(crate) const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

pub(crate) struct ParsedToken {
    pub(crate) slot: u32,
    pub(crate) generation: u64,
}
