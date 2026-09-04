use crate::generation::BindingGeneration;
use crate::{XllError, XllResult};
use std::cell::RefCell;

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
    let (chunks, _) = encoded.as_bytes().as_chunks::<2>();
    for (index, pair) in chunks.iter().enumerate() {
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

/// The authenticated identity of one formula-owned handle binding.
///
/// The wire token is only one representation of this identity. Keeping the
/// slot and generation together prevents callers from accidentally mixing a
/// slot from one token with the generation from another token.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct HandleId {
    pub(crate) slot: u32,
    pub(crate) generation: BindingGeneration,
}

/// Session-scoped identity of the shared object behind one or more formula
/// bindings. The session namespace prevents object identities from being
/// confused after a runtime generation is reopened.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ObjectId {
    session: u64,
    sequence: u64,
}

impl ObjectId {
    pub(crate) const fn new(session: u64, sequence: u64) -> Self {
        Self { session, sequence }
    }

    pub(crate) const fn session(self) -> u64 {
        self.session
    }

    pub(crate) const fn sequence(self) -> u64 {
        self.sequence
    }
}

/// A raw token received at an Excel/runtime boundary.
///
/// Keeping the borrowed string wrapped prevents syntax parsing, MAC
/// verification, and registry liveness checks from being conflated in APIs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HandleToken<'a> {
    raw: &'a str,
}

impl<'a> HandleToken<'a> {
    pub(crate) const fn new(raw: &'a str) -> Self {
        Self { raw }
    }

    pub(crate) const fn as_str(self) -> &'a str {
        self.raw
    }
}

/// A syntactically valid token whose fields have been decoded but not yet
/// authenticated against this registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ParsedHandleToken {
    pub(crate) session: u64,
    pub(crate) id: HandleId,
    pub(crate) tag: [u8; 16],
}

/// A token that passed syntax, session, and MAC verification.
///
/// It is intentionally distinct from the raw Excel string and from a live
/// registry lookup. A verified token can still be stale or have the wrong
/// Rust type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedHandleToken {
    pub(crate) id: HandleId,
}

/// Token syntax, authentication, and the small thread-local verification
/// cache.  Binding liveness is intentionally outside this type: callers must
/// still resolve the authenticated `HandleId` through the binding table.
pub(crate) struct TokenCodec {
    pub(crate) session: u64,
    pub(crate) secret: [u8; 32],
}

impl TokenCodec {
    pub(crate) const fn new(session: u64, secret: [u8; 32]) -> Self {
        Self { session, secret }
    }

    pub(crate) fn format(&self, id: HandleId) -> String {
        let tag = encode_tag(&self.authentication_tag(id));
        format!(
            "xllh:3:{:016x}:{:08x}:{:016x}:{tag}",
            self.session,
            id.slot,
            id.generation.get()
        )
    }

    pub(crate) fn parse(
        &self,
        registry_address: usize,
        token: HandleToken<'_>,
    ) -> XllResult<VerifiedHandleToken> {
        if let Some(id) = verified_token_cache_lookup(
            registry_address,
            self.session,
            &self.secret,
            token.as_str(),
        ) {
            return Ok(VerifiedHandleToken { id });
        }

        let parsed = self.parse_uncached(token)?;
        let verified = self.verify(parsed)?;
        verified_token_cache_store(
            registry_address,
            self.session,
            &self.secret,
            token.as_str(),
            verified.id,
        );
        Ok(verified)
    }

    fn parse_uncached(&self, token: HandleToken<'_>) -> XllResult<ParsedHandleToken> {
        let mut fields = token.as_str().splitn(7, ':');
        let prefix = fields.next().ok_or(XllError::InvalidHandle)?;
        let version = fields.next().ok_or(XllError::InvalidHandle)?;
        let session = fields.next().ok_or(XllError::InvalidHandle)?;
        let slot = fields.next().ok_or(XllError::InvalidHandle)?;
        let generation = fields.next().ok_or(XllError::InvalidHandle)?;
        let tag = fields.next().ok_or(XllError::InvalidHandle)?;
        if fields.next().is_some()
            || prefix != "xllh"
            || version != "3"
            || session.len() != 16
            || slot.len() != 8
            || generation.len() != 16
            || tag.len() != 32
        {
            return Err(XllError::InvalidHandle);
        }
        let session = u64::from_str_radix(session, 16).map_err(|_| XllError::InvalidHandle)?;
        let slot = u32::from_str_radix(slot, 16).map_err(|_| XllError::InvalidHandle)?;
        let generation = u64::from_str_radix(generation, 16)
            .ok()
            .and_then(BindingGeneration::new)
            .ok_or(XllError::InvalidHandle)?;
        let tag = decode_tag(tag).ok_or(XllError::InvalidHandle)?;
        Ok(ParsedHandleToken {
            session,
            id: HandleId { slot, generation },
            tag,
        })
    }

    fn verify(&self, parsed: ParsedHandleToken) -> XllResult<VerifiedHandleToken> {
        let expected = self.authentication_tag(parsed.id);
        if parsed.session != self.session
            || !constant_time_eq::constant_time_eq(&parsed.tag, &expected)
        {
            return Err(XllError::InvalidHandle);
        }
        Ok(VerifiedHandleToken { id: parsed.id })
    }

    pub(crate) fn authentication_tag(&self, id: HandleId) -> [u8; 16] {
        let mut mac = blake3::Hasher::new_keyed(&self.secret);
        mac.update(b"xlfn-handle-token-v1\0");
        mac.update(&self.session.to_le_bytes());
        mac.update(&id.slot.to_le_bytes());
        mac.update(&id.generation.get().to_le_bytes());
        mac.finalize().as_bytes()[..16]
            .try_into()
            .expect("the BLAKE3 output contains a 128-bit tag")
    }
}

pub(crate) const HANDLE_TOKEN_LENGTH: usize = 82;
const VERIFIED_TOKEN_CACHE_SIZE: usize = 16;

struct VerifiedTokenCacheEntry {
    registry_address: usize,
    session: u64,
    secret: [u8; 32],
    token: [u8; HANDLE_TOKEN_LENGTH],
    id: HandleId,
}

thread_local! {
    static VERIFIED_TOKEN_CACHE: RefCell<[
        Option<VerifiedTokenCacheEntry>;
        VERIFIED_TOKEN_CACHE_SIZE
    ]> = const { RefCell::new([const { None }; VERIFIED_TOKEN_CACHE_SIZE]) };
}

#[inline]
fn verified_token_cache_index(bytes: &[u8]) -> Option<usize> {
    if bytes.len() != HANDLE_TOKEN_LENGTH {
        return None;
    }

    // The final token byte is one hex nibble of the authenticated tag. It is
    // used directly to choose a direct-mapped cache bucket; the complete token,
    // registry identity, and secret are still checked before accepting a hit.
    let nibble = hex_nibble(bytes[HANDLE_TOKEN_LENGTH - 1])?;
    Some(usize::from(nibble))
}

pub(crate) fn verified_token_cache_lookup(
    registry_address: usize,
    session: u64,
    secret: &[u8; 32],
    token: &str,
) -> Option<HandleId> {
    let bytes = token.as_bytes();
    let index = verified_token_cache_index(bytes)?;
    VERIFIED_TOKEN_CACHE.with(|cache| {
        let cache = cache.borrow();
        cache[index].as_ref().and_then(|entry| {
            (entry.registry_address == registry_address
                && entry.session == session
                && entry.secret == *secret
                && entry.token.as_slice() == bytes)
                .then_some(entry.id)
        })
    })
}

pub(crate) fn verified_token_cache_store(
    registry_address: usize,
    session: u64,
    secret: &[u8; 32],
    token: &str,
    id: HandleId,
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
            id,
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_token_cache_index_distinguishes_all_16_nibbles() {
        let mut token = vec![b'a'; HANDLE_TOKEN_LENGTH];
        let hex_chars = b"0123456789abcdef";
        let mut seen = std::collections::HashSet::new();
        for &ch in hex_chars {
            token[HANDLE_TOKEN_LENGTH - 1] = ch;
            let index = verified_token_cache_index(&token).expect("valid hex nibble index");
            assert!(index < VERIFIED_TOKEN_CACHE_SIZE);
            assert!(
                seen.insert(index),
                "no aliasing between distinct nibbles: {index}"
            );
        }
        assert_eq!(seen.len(), 16);

        // Specifically verify nibble '0' vs '8' (which previously aliased under & 7):
        token[HANDLE_TOKEN_LENGTH - 1] = b'0';
        let idx_0 = verified_token_cache_index(&token).unwrap();
        token[HANDLE_TOKEN_LENGTH - 1] = b'8';
        let idx_8 = verified_token_cache_index(&token).unwrap();
        assert_ne!(idx_0, idx_8);
    }
}
