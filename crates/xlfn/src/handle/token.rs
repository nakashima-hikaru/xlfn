use crate::generation::BindingGeneration;
use crate::{XllError, XllResult};
use std::cell::RefCell;

const HEX: &[u8; 16] = b"0123456789abcdef";

fn write_hex(mut value: u64, encoded: &mut [u8]) {
    for byte in encoded.iter_mut().rev() {
        *byte = HEX[(value & 0xf) as usize];
        value >>= 4;
    }
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
        let mut token = vec![b':'; HANDLE_TOKEN_LENGTH];
        token[..7].copy_from_slice(b"xllh:3:");
        write_hex(self.session, &mut token[7..23]);
        write_hex(u64::from(id.slot), &mut token[24..32]);
        write_hex(id.generation.get(), &mut token[33..49]);
        let (pairs, _) = token[50..].as_chunks_mut::<2>();
        for (byte, pair) in self.authentication_tag(id).iter().zip(pairs) {
            pair[0] = HEX[(byte >> 4) as usize];
            pair[1] = HEX[(byte & 0xf) as usize];
        }
        String::from_utf8(token).expect("handle tokens contain only ASCII")
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
const VERIFIED_TOKEN_CACHE_SETS: usize = 16;
const VERIFIED_TOKEN_CACHE_WAYS: usize = 4;

struct VerifiedTokenCache {
    sets: [[Option<VerifiedTokenCacheEntry>; VERIFIED_TOKEN_CACHE_WAYS]; VERIFIED_TOKEN_CACHE_SETS],
    victims: [u8; VERIFIED_TOKEN_CACHE_SETS],
}

impl VerifiedTokenCache {
    const fn new() -> Self {
        Self {
            sets: [const { [const { None }; VERIFIED_TOKEN_CACHE_WAYS] };
                VERIFIED_TOKEN_CACHE_SETS],
            victims: [0; VERIFIED_TOKEN_CACHE_SETS],
        }
    }
}

#[cfg(feature = "bench-internals")]
pub(super) const VERIFIED_TOKEN_CACHE_STORAGE_BYTES: usize =
    std::mem::size_of::<RefCell<VerifiedTokenCache>>();

struct VerifiedTokenCacheEntry {
    registry_address: usize,
    session: u64,
    secret: [u8; 32],
    token: [u8; HANDLE_TOKEN_LENGTH],
    id: HandleId,
}

thread_local! {
    static VERIFIED_TOKEN_CACHE: RefCell<VerifiedTokenCache> = const { RefCell::new(VerifiedTokenCache::new()) };
}

#[inline]
fn verified_token_cache_index(bytes: &[u8]) -> Option<usize> {
    if bytes.len() != HANDLE_TOKEN_LENGTH {
        return None;
    }

    // The final token byte is one hex nibble of the authenticated tag. It
    // selects one of 16 sets. Keeping all 16 sets avoids introducing conflicts
    // between previously distinct tag nibbles when adding associativity. The
    // complete token, registry identity and secret are checked on every hit.
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
        cache.sets[index].iter().find_map(|entry| {
            let entry = entry.as_ref()?;
            (entry.registry_address == registry_address
                && entry.session == session
                && entry.token.as_slice() == bytes
                && entry.secret == *secret)
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
        let mut cache = cache.borrow_mut();
        let victim = usize::from(cache.victims[index]);
        cache.victims[index] = ((victim + 1) % VERIFIED_TOKEN_CACHE_WAYS) as u8;
        cache.sets[index][victim] = Some(VerifiedTokenCacheEntry {
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

    fn clear_cache() {
        VERIFIED_TOKEN_CACHE.with(|cache| *cache.borrow_mut() = VerifiedTokenCache::new());
    }

    #[test]
    fn formatted_tokens_preserve_wire_bytes_and_round_trip() {
        for session in [0, 1, u64::MAX] {
            let codec = TokenCodec::new(session, [19; 32]);
            for slot in [0, 1, u32::MAX] {
                for generation in [1, 2, u64::MAX] {
                    let id = HandleId {
                        slot,
                        generation: BindingGeneration::new(generation).unwrap(),
                    };
                    let tag = codec
                        .authentication_tag(id)
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                    let expected =
                        format!("xllh:3:{session:016x}:{slot:08x}:{generation:016x}:{tag}");
                    let token = codec.format(id);
                    assert_eq!(token, expected);
                    assert_eq!(token.len(), HANDLE_TOKEN_LENGTH);
                    assert_eq!(
                        codec
                            .verify(codec.parse_uncached(HandleToken::new(&token)).unwrap())
                            .unwrap()
                            .id,
                        id
                    );
                }
            }
        }
    }

    #[test]
    fn cache_retains_four_colliding_authenticated_tokens_without_merging_sets() {
        clear_cache();
        let codec = TokenCodec::new(7, [19; 32]);
        let address = std::ptr::from_ref(&codec).addr();
        let colliding: Vec<_> = (0..1024)
            .filter_map(|slot| {
                let id = HandleId {
                    slot,
                    generation: BindingGeneration::ONE,
                };
                let token = codec.format(id);
                token.ends_with('0').then_some((token, id))
            })
            .take(5)
            .collect();
        for (token, id) in &colliding[..4] {
            assert_eq!(
                codec.parse(address, HandleToken::new(token)).unwrap().id,
                *id
            );
        }
        for (token, id) in &colliding[..4] {
            assert_eq!(
                verified_token_cache_lookup(address, codec.session, &codec.secret, token),
                Some(*id)
            );
        }
        codec
            .parse(address, HandleToken::new(&colliding[4].0))
            .unwrap();
        assert_eq!(
            verified_token_cache_lookup(address, codec.session, &codec.secret, &colliding[0].0),
            None
        );
        for (token, id) in &colliding[1..] {
            assert_eq!(
                verified_token_cache_lookup(address, codec.session, &codec.secret, token),
                Some(*id)
            );
        }
    }

    #[test]
    fn cached_verification_checks_entire_token_and_registry_identity() {
        clear_cache();
        let codec = TokenCodec::new(7, [19; 32]);
        let address = std::ptr::from_ref(&codec).addr();
        let id = HandleId {
            slot: 17,
            generation: BindingGeneration::ONE,
        };
        let token = codec.format(id);
        codec.parse(address, HandleToken::new(&token)).unwrap();
        assert!(
            verified_token_cache_lookup(address + 1, codec.session, &codec.secret, &token)
                .is_none()
        );
        assert!(
            verified_token_cache_lookup(address, codec.session + 1, &codec.secret, &token)
                .is_none()
        );
        assert!(verified_token_cache_lookup(address, codec.session, &[20; 32], &token).is_none());
        for index in 0..HANDLE_TOKEN_LENGTH - 1 {
            let mut tampered = token.clone().into_bytes();
            tampered[index] = if tampered[index] == b'0' { b'1' } else { b'0' };
            let tampered = String::from_utf8(tampered).unwrap();
            assert!(
                verified_token_cache_lookup(address, codec.session, &codec.secret, &tampered)
                    .is_none()
            );
            assert!(
                codec.parse(address, HandleToken::new(&tampered)).is_err(),
                "byte {index}"
            );
        }
    }

    #[test]
    fn verified_token_cache_index_distinguishes_all_16_nibbles() {
        let mut token = vec![b'a'; HANDLE_TOKEN_LENGTH];
        let hex_chars = b"0123456789abcdef";
        let mut seen = std::collections::HashSet::new();
        for &ch in hex_chars {
            token[HANDLE_TOKEN_LENGTH - 1] = ch;
            let index = verified_token_cache_index(&token).expect("valid hex nibble index");
            assert!(index < VERIFIED_TOKEN_CACHE_SETS);
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
