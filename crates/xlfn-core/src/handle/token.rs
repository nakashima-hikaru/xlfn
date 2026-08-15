use super::*;
use std::cell::RefCell;

pub(crate) fn drop_handle_objects(
    values: impl IntoIterator<Item = Arc<HandleObject>>,
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

#[derive(Clone, Copy)]
pub(crate) struct ParsedToken {
    pub(crate) slot: u32,
    pub(crate) generation: u64,
}

pub(crate) const HANDLE_TOKEN_LENGTH: usize = 82;
const VERIFIED_TOKEN_CACHE_SIZE: usize = 8;

#[derive(Clone, Copy)]
struct VerifiedTokenCacheEntry {
    registry_address: usize,
    session: u64,
    secret: [u8; 32],
    token: [u8; HANDLE_TOKEN_LENGTH],
    parsed: ParsedToken,
}

thread_local! {
    static VERIFIED_TOKEN_CACHE: RefCell<[
        Option<VerifiedTokenCacheEntry>;
        VERIFIED_TOKEN_CACHE_SIZE
    ]> = const { RefCell::new([None; VERIFIED_TOKEN_CACHE_SIZE]) };
}

#[inline]
fn verified_token_cache_index(bytes: &[u8]) -> Option<usize> {
    if bytes.len() != HANDLE_TOKEN_LENGTH {
        return None;
    }

    // The final token byte is one hex nibble of the authenticated tag. It is
    // used only to choose a direct-mapped cache bucket; the complete token,
    // registry identity, and secret are still checked before accepting a hit.
    let nibble = hex_nibble(bytes[HANDLE_TOKEN_LENGTH - 1])?;
    Some(usize::from(nibble) & (VERIFIED_TOKEN_CACHE_SIZE - 1))
}

pub(crate) fn verified_token_cache_lookup(
    registry_address: usize,
    session: u64,
    secret: &[u8; 32],
    token: &str,
) -> Option<ParsedToken> {
    let bytes = token.as_bytes();
    let index = verified_token_cache_index(bytes)?;
    VERIFIED_TOKEN_CACHE.with(|cache| {
        let entry = cache.borrow()[index];
        entry.and_then(|entry| {
            (entry.registry_address == registry_address
                && entry.session == session
                && entry.secret == *secret
                && entry.token.as_slice() == bytes)
                .then_some(entry.parsed)
        })
    })
}

pub(crate) fn verified_token_cache_store(
    registry_address: usize,
    session: u64,
    secret: &[u8; 32],
    token: &str,
    parsed: ParsedToken,
) {
    let bytes = token.as_bytes();
    let Some(index) = verified_token_cache_index(bytes) else {
        return;
    };
    let mut token_bytes = [0_u8; HANDLE_TOKEN_LENGTH];
    token_bytes.copy_from_slice(bytes);
    VERIFIED_TOKEN_CACHE.with(|cache| {
        cache.borrow_mut()[index] = Some(VerifiedTokenCacheEntry {
            registry_address,
            session,
            secret: *secret,
            token: token_bytes,
            parsed,
        });
    });
}
