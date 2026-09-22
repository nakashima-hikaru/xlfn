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

fn decode_tag(encoded: &[u8]) -> Option<[u8; 16]> {
    if encoded.len() != 32 {
        return None;
    }
    let mut tag = [0_u8; 16];
    let (chunks, _) = encoded.as_chunks::<2>();
    for (index, pair) in chunks.iter().enumerate() {
        tag[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Some(tag)
}

const fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn decode_hex(encoded: &[u8]) -> Option<u64> {
    // All callers select a fixed wire field no wider than 16 hex digits.
    // Its width proves overflow impossible, without per-digit checked math.
    debug_assert!(encoded.len() <= 16);
    encoded.iter().try_fold(0, |value, &byte| {
        Some((value << 4) | u64::from(hex_nibble(byte)?))
    })
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
        let bytes = token.as_str().as_bytes();
        if bytes.len() != HANDLE_TOKEN_LENGTH
            || !bytes.starts_with(b"xllh:3:")
            || bytes[23] != b':'
            || bytes[32] != b':'
            || bytes[49] != b':'
        {
            return Err(XllError::InvalidHandle);
        }
        // The formatter and parser share one canonical, lowercase ASCII
        // grammar. Byte slices also reject non-ASCII input without risking
        // UTF-8 boundary panics on fixed-position string slicing.
        let session = decode_hex(&bytes[7..23]).ok_or(XllError::InvalidHandle)?;
        let slot = decode_hex(&bytes[24..32]).ok_or(XllError::InvalidHandle)? as u32;
        let generation = decode_hex(&bytes[33..49])
            .and_then(BindingGeneration::new)
            .ok_or(XllError::InvalidHandle)?;
        let tag = decode_tag(&bytes[50..]).ok_or(XllError::InvalidHandle)?;
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
        let mut message = [0; 41];
        message[..21].copy_from_slice(b"xlfn-handle-token-v1\0");
        message[21..29].copy_from_slice(&self.session.to_le_bytes());
        message[29..33].copy_from_slice(&id.slot.to_le_bytes());
        message[33..41].copy_from_slice(&id.generation.get().to_le_bytes());
        blake3::keyed_hash(&self.secret, &message).as_bytes()[..16]
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
    // Keep the verified pair in flat storage: embedding an aligned HandleId
    // would retain its padding beside the 82-byte wire token. Construction and
    // lookup still accept/return the one typed identity as a unit.
    slot: u32,
    generation: BindingGeneration,
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
                .then_some(HandleId {
                    slot: entry.slot,
                    generation: entry.generation,
                })
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
            slot: id.slot,
            generation: id.generation,
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
                    // Independent streaming construction checks the wire MAC
                    // bytes while production uses one fixed-layout message.
                    let mut reference = blake3::Hasher::new_keyed(&codec.secret);
                    reference.update(b"xlfn-handle-token-v1\0");
                    reference.update(&session.to_le_bytes());
                    reference.update(&slot.to_le_bytes());
                    reference.update(&generation.to_le_bytes());
                    let expected_tag = reference.finalize();
                    assert_eq!(codec.authentication_tag(id), expected_tag.as_bytes()[..16]);
                    let tag = expected_tag.as_bytes()[..16]
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
    fn fixed_wire_parser_rejects_noncanonical_and_non_ascii_tokens_without_panicking() {
        let codec = TokenCodec::new(0xab, [19; 32]);
        let id = HandleId {
            slot: 0xab,
            generation: BindingGeneration::new(0xab).unwrap(),
        };
        let token = codec.format(id);
        for length in 0..HANDLE_TOKEN_LENGTH {
            assert!(
                codec
                    .parse_uncached(HandleToken::new(&token[..length]))
                    .is_err()
            );
        }
        for field in [7..23, 24..32, 33..49] {
            let mut uppercase = token.clone();
            uppercase.replace_range(field.clone(), &token[field.clone()].to_ascii_uppercase());
            assert!(codec.parse_uncached(HandleToken::new(&uppercase)).is_err());
            let mut signed = token.clone();
            signed.replace_range(field.start..field.start + 1, "+");
            assert!(codec.parse_uncached(HandleToken::new(&signed)).is_err());
        }
        for position in 0..HANDLE_TOKEN_LENGTH - 1 {
            let mut non_ascii = token.clone();
            // Keep the total byte length unchanged and place a UTF-8 code
            // point across every possible field / separator boundary.
            non_ascii.replace_range(position..position + 2, "é");
            assert!(codec.parse_uncached(HandleToken::new(&non_ascii)).is_err());
        }
        let mut zero_generation = token.clone();
        zero_generation.replace_range(33..49, "0000000000000000");
        assert!(
            codec
                .parse_uncached(HandleToken::new(&zero_generation))
                .is_err()
        );
        assert!(
            codec
                .parse_uncached(HandleToken::new(&(token + ":")))
                .is_err()
        );
    }

    #[test]
    fn cache_keeps_distinct_registry_identities_resident_across_switches() {
        clear_cache();
        let codecs = [TokenCodec::new(7, [19; 32]), TokenCodec::new(8, [20; 32])];
        let id = HandleId {
            slot: 7,
            generation: BindingGeneration::ONE,
        };
        let tokens = codecs.each_ref().map(|codec| codec.format(id));
        for _ in 0..8 {
            for (codec, token) in codecs.iter().zip(&tokens) {
                assert_eq!(
                    codec
                        .parse(std::ptr::from_ref(codec).addr(), HandleToken::new(token))
                        .unwrap()
                        .id,
                    id
                );
            }
        }
        for (codec, token) in codecs.iter().zip(&tokens) {
            assert_eq!(
                verified_token_cache_lookup(
                    std::ptr::from_ref(codec).addr(),
                    codec.session,
                    &codec.secret,
                    token
                ),
                Some(id)
            );
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
        let mut seen = rustc_hash::FxHashSet::default();
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
