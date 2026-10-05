use super::source::SourceHandleId;
use crate::{XllError, XllResult};
use rustc_hash::FxHasher;
use smol_str::SmolStr;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;

pub(crate) const MAX_RTD_TOPIC_PARTS: usize = 253;
pub(crate) const MAX_RTD_TOPIC_BYTES: usize = 1024 * 1024;
pub(crate) const DEFAULT_MAX_RTD_PENDING: usize = 4096;
pub(crate) const DEFAULT_MAX_RTD_ACTIVE: usize = 4096;
pub(crate) const DEFAULT_MAX_RTD_QUEUED_UPDATES: usize = 4096;
pub(crate) const DEFAULT_MAX_RTD_SOURCE_IDS: usize = 4096;
pub(crate) const DEFAULT_MAX_RTD_TOTAL_TOPIC_BYTES: usize = 64 * 1024 * 1024;

/// Capacity for one RTD resource class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RtdCapacity {
    /// Do not admit the corresponding RTD resource.
    Disabled,
    /// Admit at most this many live resources.
    Bounded(NonZeroUsize),
}

impl RtdCapacity {
    #[must_use]
    /// Disables admission for this resource.
    pub const fn disabled() -> Self {
        Self::Disabled
    }

    #[must_use]
    /// Admits at most the given positive number of resources.
    pub const fn bounded(value: NonZeroUsize) -> Self {
        Self::Bounded(value)
    }

    #[must_use]
    /// Disables admission when `value` is zero; positive values are finite bounds.
    ///
    /// Zero never means unlimited capacity.
    pub const fn disabled_if_zero(value: usize) -> Self {
        match NonZeroUsize::new(value) {
            Some(value) => Self::Bounded(value),
            None => Self::Disabled,
        }
    }

    #[must_use]
    /// Returns the finite bound, or zero for disabled admission.
    pub const fn get(self) -> usize {
        match self {
            Self::Disabled => 0,
            Self::Bounded(value) => value.get(),
        }
    }

    #[must_use]
    /// Returns whether admission is disabled.
    pub const fn is_disabled(self) -> bool {
        matches!(self, Self::Disabled)
    }
}

/// Resource limits for one add-in's RTD subscription runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RtdLimits {
    pub(crate) max_pending: RtdCapacity,
    pub(crate) max_active: RtdCapacity,
    pub(crate) max_queued_updates: RtdCapacity,
    pub(crate) max_source_ids: RtdCapacity,
    pub(crate) max_total_topic_bytes: RtdCapacity,
}

impl RtdLimits {
    #[must_use]
    /// Returns the default finite subscription, queue, source, and topic-byte limits.
    pub const fn standard() -> Self {
        Self {
            max_pending: RtdCapacity::disabled_if_zero(DEFAULT_MAX_RTD_PENDING),
            max_active: RtdCapacity::disabled_if_zero(DEFAULT_MAX_RTD_ACTIVE),
            max_queued_updates: RtdCapacity::disabled_if_zero(DEFAULT_MAX_RTD_QUEUED_UPDATES),
            max_source_ids: RtdCapacity::disabled_if_zero(DEFAULT_MAX_RTD_SOURCE_IDS),
            max_total_topic_bytes: RtdCapacity::disabled_if_zero(DEFAULT_MAX_RTD_TOTAL_TOPIC_BYTES),
        }
    }

    #[must_use]
    /// Sets the capacity for subscriptions waiting to connect.
    pub const fn with_max_pending(mut self, value: RtdCapacity) -> Self {
        self.max_pending = value;
        self
    }

    #[must_use]
    /// Bound live subscriptions plus disconnected subscriptions whose source
    /// cleanup has not completed. Capacity returns after the sink-user barrier,
    /// so terminal disconnect always has reserved cleanup capacity.
    pub const fn with_max_active(mut self, value: RtdCapacity) -> Self {
        self.max_active = value;
        self
    }

    #[must_use]
    /// Sets the capacity for queued topic updates.
    pub const fn with_max_queued_updates(mut self, value: RtdCapacity) -> Self {
        self.max_queued_updates = value;
        self
    }

    #[must_use]
    /// Sets the capacity for registered source identities.
    pub const fn with_max_source_ids(mut self, value: RtdCapacity) -> Self {
        self.max_source_ids = value;
        self
    }

    #[must_use]
    /// Sets the aggregate byte budget for retained topic parts.
    pub const fn with_max_total_topic_bytes(mut self, value: RtdCapacity) -> Self {
        self.max_total_topic_bytes = value;
        self
    }

    #[must_use]
    /// Returns the pending-subscription capacity.
    pub const fn max_pending(&self) -> RtdCapacity {
        self.max_pending
    }

    #[must_use]
    /// Capacity for live subscriptions and source owners awaiting cleanup.
    pub const fn max_active(&self) -> RtdCapacity {
        self.max_active
    }

    #[must_use]
    /// Returns the queued-update capacity.
    pub const fn max_queued_updates(&self) -> RtdCapacity {
        self.max_queued_updates
    }

    #[must_use]
    /// Returns the source-identity capacity.
    pub const fn max_source_ids(&self) -> RtdCapacity {
        self.max_source_ids
    }

    #[must_use]
    /// Returns the aggregate retained topic-byte budget.
    pub const fn max_total_topic_bytes(&self) -> RtdCapacity {
        self.max_total_topic_bytes
    }
}

impl Default for RtdLimits {
    fn default() -> Self {
        Self::standard()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct TopicId(pub(crate) i32);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SourceId(pub(crate) SourceHandleId);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SubscriptionIdentityKey {
    pub(crate) source_id: SourceId,
    pub(crate) topic_hash: u64,
}

impl SubscriptionIdentityKey {
    pub(crate) fn new(source_id: SourceId, topic: &RtdTopic) -> Self {
        Self {
            source_id,
            topic_hash: topic.identity_hash(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct SubscriptionId(pub(crate) u64);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SubscriptionKey {
    runtime_id: u64,
    subscription_id: u64,
}

/// Fixed-width ASCII transport representation, kept inline during observation.
pub(crate) struct SubscriptionTransportKey([u8; 43]);

impl SubscriptionTransportKey {
    pub(crate) fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("subscription transport contains only ASCII")
    }
}

impl SubscriptionKey {
    pub(crate) const fn from_internal(runtime_id: u64, id: SubscriptionId) -> Self {
        Self {
            runtime_id,
            subscription_id: id.0,
        }
    }

    #[cfg(test)]
    pub(crate) const fn from_allocated_id(runtime_id: u64, subscription_id: u64) -> Self {
        Self {
            runtime_id,
            subscription_id,
        }
    }

    pub(crate) fn validate_runtime(self, expected_runtime_id: u64) -> Option<SubscriptionId> {
        if self.runtime_id == expected_runtime_id {
            Some(SubscriptionId(self.subscription_id))
        } else {
            None
        }
    }

    pub(crate) fn to_transport(self) -> SubscriptionTransportKey {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = *b"stream:v1:0000000000000000:0000000000000000";
        for offset in 0..16 {
            let shift = 4 * (15 - offset);
            encoded[10 + offset] = HEX[((self.runtime_id >> shift) & 0xf) as usize];
            encoded[27 + offset] = HEX[((self.subscription_id >> shift) & 0xf) as usize];
        }
        SubscriptionTransportKey(encoded)
    }

    pub(crate) fn parse_transport(value: &str) -> XllResult<Self> {
        const PREFIX: &str = "stream:v1:";

        let Some(rest) = value.strip_prefix(PREFIX) else {
            return Err(XllError::InvalidHandle);
        };

        let Some((runtime, subscription)) = rest.split_once(':') else {
            return Err(XllError::InvalidHandle);
        };

        let Some(runtime_id) = parse_fixed_hex(runtime) else {
            return Err(XllError::InvalidHandle);
        };
        let Some(subscription_id) = parse_fixed_hex(subscription) else {
            return Err(XllError::InvalidHandle);
        };

        Ok(Self {
            runtime_id,
            subscription_id,
        })
    }
}

fn parse_fixed_hex(value: &str) -> Option<u64> {
    (value.len() == 16 && value.as_bytes().iter().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| u64::from_str_radix(value, 16).ok())
        .flatten()
}

#[derive(Clone, Debug)]
/// An owned, validated sequence of topic strings used for subscription identity.
pub struct RtdTopic {
    parts: Box<[SmolStr]>,
    byte_len: usize,
    hash: u64,
}

impl RtdTopic {
    /// Validates and owns topic parts, rejecting excessive part counts or byte lengths.
    pub fn new(parts: impl IntoIterator<Item = impl AsRef<str>>) -> XllResult<Self> {
        let parts = parts.into_iter();
        // Arrays, slices and mapped collections expose their length. Avoid
        // growth followed by boxed-slice shrinking on these common inputs,
        // while bounding reservations from arbitrary iterator size hints.
        let mut normalized = Vec::with_capacity(parts.size_hint().0.min(MAX_RTD_TOPIC_PARTS));
        for part in parts {
            if normalized.len() >= MAX_RTD_TOPIC_PARTS {
                return Err(XllError::input(
                    "RTD topic",
                    crate::error::InputError::TooLarge {
                        limit: MAX_RTD_TOPIC_PARTS,
                        actual: normalized.len().saturating_add(1),
                    },
                ));
            }
            let part = SmolStr::new(part.as_ref());
            if part.is_empty() {
                return Err(XllError::input(
                    "RTD topic",
                    crate::error::InputError::Malformed("RTD topics require non-empty parts"),
                ));
            }
            normalized.push(part);
        }
        if normalized.is_empty() {
            return Err(XllError::input(
                "RTD topic",
                crate::error::InputError::Malformed("RTD topics require non-empty parts"),
            ));
        }
        let TopicMetadata { byte_len, hash } =
            validate_topic_parts(normalized.iter().map(SmolStr::as_str))?;
        Ok(Self {
            parts: normalized.into_boxed_slice(),
            byte_len,
            hash,
        })
    }

    /// Creates a validated topic with one part.
    pub fn single(part: impl AsRef<str>) -> XllResult<Self> {
        Self::new([part])
    }

    /// Returns the number of topic parts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// Returns whether the topic has no parts. Valid topics are always non-empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Returns one topic part, or `None` if the index is out of bounds.
    #[must_use]
    pub fn part(&self, index: usize) -> Option<&str> {
        self.parts.get(index).map(SmolStr::as_str)
    }

    /// Borrows the canonical parts in order without allocating.
    #[must_use]
    pub fn parts(&self) -> RtdTopicParts<'_> {
        RtdTopicParts {
            inner: self.parts.iter(),
        }
    }

    pub(crate) fn borrowed(&self) -> BorrowedTopicParts<'_, SmolStr> {
        BorrowedTopicParts {
            parts: &self.parts,
            metadata: TopicMetadata {
                byte_len: self.byte_len,
                hash: self.hash,
            },
        }
    }

    pub(crate) const fn identity_hash(&self) -> u64 {
        self.hash
    }

    #[cfg(test)]
    pub(crate) fn with_test_identity_hash(mut self, hash: u64) -> Self {
        self.hash = hash;
        self
    }

    pub(crate) fn byte_len(&self) -> usize {
        self.byte_len
    }
}

/// Borrowed topic parts in their canonical order.
#[derive(Clone, Debug)]
pub struct RtdTopicParts<'a> {
    inner: std::slice::Iter<'a, SmolStr>,
}

impl<'a> Iterator for RtdTopicParts<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(SmolStr::as_str)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl ExactSizeIterator for RtdTopicParts<'_> {}

impl std::iter::FusedIterator for RtdTopicParts<'_> {}

impl<'a> IntoIterator for &'a RtdTopic {
    type Item = &'a str;
    type IntoIter = RtdTopicParts<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.parts()
    }
}

/// Borrowed, stable topic storage accepted by [`RtdCallContext::subscribe`](crate::rtd::RtdCallContext::subscribe).
///
/// Implemented for [`RtdTopic`], arrays, slices, and `Vec` whose elements are
/// `String` or references to `str` or `String`. References to these containers
/// are also supported. This trait is sealed: arbitrary iterators and custom
/// string conversions must first be validated and owned by [`RtdTopic::new`].
/// This keeps validation, subscription identity, and retained byte accounting
/// tied to the same immutable topic parts without allocating for reuse.
///
/// ```compile_fail
/// use std::cell::Cell;
/// use xlfn::rtd::RtdTopicInput;
/// fn borrowed_topic(_: &impl RtdTopicInput) {}
/// let visits = Cell::new(0);
/// let changing = [()].into_iter().map(|_| {
///     visits.set(visits.get() + 1);
///     if visits.get() <= 3 { "valid" } else { "" }
/// });
/// borrowed_topic(&changing);
/// ```
#[allow(
    private_bounds,
    reason = "Topic inputs are sealed to stable borrowed storage"
)]
pub trait RtdTopicInput: input::Input {}

impl<Parts: input::Input + ?Sized> RtdTopicInput for Parts {}

mod input {
    use super::{BorrowedTopicParts, RtdTopic};
    use crate::XllResult;

    pub(crate) trait Input {
        type Part: AsRef<str>;

        fn prepare_parts(&self) -> XllResult<BorrowedTopicParts<'_, Self::Part>>;
    }

    // Only built-in immutable string views may be reread while matching a
    // subscription under the catalog lock. User-defined AsRef implementations
    // could return a different string on each visit or reenter the catalog.
    trait StablePart: AsRef<str> {}

    impl StablePart for str {}
    impl StablePart for String {}
    impl<Part: StablePart + ?Sized> StablePart for &Part {}

    impl Input for RtdTopic {
        type Part = smol_str::SmolStr;

        fn prepare_parts(&self) -> XllResult<BorrowedTopicParts<'_, Self::Part>> {
            Ok(self.borrowed())
        }
    }

    impl<Part: StablePart> Input for [Part] {
        type Part = Part;

        fn prepare_parts(&self) -> XllResult<BorrowedTopicParts<'_, Part>> {
            BorrowedTopicParts::new(self)
        }
    }

    impl<Part: StablePart, const N: usize> Input for [Part; N] {
        type Part = Part;

        fn prepare_parts(&self) -> XllResult<BorrowedTopicParts<'_, Part>> {
            BorrowedTopicParts::new(self)
        }
    }

    impl<Part: StablePart> Input for Vec<Part> {
        type Part = Part;

        fn prepare_parts(&self) -> XllResult<BorrowedTopicParts<'_, Part>> {
            BorrowedTopicParts::new(self)
        }
    }

    impl<Parts: Input + ?Sized> Input for &Parts {
        type Part = Parts::Part;

        fn prepare_parts(&self) -> XllResult<BorrowedTopicParts<'_, Self::Part>> {
            (*self).prepare_parts()
        }
    }
}

impl PartialEq for RtdTopic {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.parts == other.parts
    }
}

impl Eq for RtdTopic {}

impl Hash for RtdTopic {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TopicMetadata {
    byte_len: usize,
    hash: u64,
}

fn validate_topic_parts<'a>(
    parts: impl Clone + ExactSizeIterator<Item = &'a str>,
) -> XllResult<TopicMetadata> {
    if parts.len() == 0 {
        return Err(XllError::input(
            "RTD topic",
            crate::error::InputError::Malformed("RTD topics require non-empty parts"),
        ));
    }
    // Keep the owning constructor's error precedence: an empty part among
    // the first 253 wins, but the 254th is rejected before inspecting it.
    for (index, part) in parts.clone().enumerate() {
        if index >= MAX_RTD_TOPIC_PARTS {
            return Err(XllError::input(
                "RTD topic",
                crate::error::InputError::TooLarge {
                    limit: MAX_RTD_TOPIC_PARTS,
                    actual: index.saturating_add(1),
                },
            ));
        }
        if part.is_empty() {
            return Err(XllError::input(
                "RTD topic",
                crate::error::InputError::Malformed("RTD topics require non-empty parts"),
            ));
        }
    }
    let mut total_bytes = 0_usize;
    let mut hasher = FxHasher::default();
    parts.len().hash(&mut hasher);
    for part in parts.clone() {
        total_bytes = total_bytes.checked_add(part.len()).ok_or_else(|| {
            XllError::input(
                "RTD topic",
                crate::error::InputError::TooLarge {
                    limit: MAX_RTD_TOPIC_BYTES,
                    actual: usize::MAX,
                },
            )
        })?;
        part.hash(&mut hasher);
    }
    if total_bytes > MAX_RTD_TOPIC_BYTES {
        return Err(XllError::input(
            "RTD topic",
            crate::error::InputError::TooLarge {
                limit: MAX_RTD_TOPIC_BYTES,
                actual: total_bytes,
            },
        ));
    }
    for part in parts {
        // Every Unicode scalar needs at most as many UTF-16 units as UTF-8
        // bytes. Short parts are therefore valid without decoding their text.
        if part.len() <= crate::utf16::EXCEL_STRING_LIMIT {
            continue;
        }
        let length = part.encode_utf16().count();
        if length > crate::utf16::EXCEL_STRING_LIMIT {
            return Err(XllError::input(
                "RTD topic",
                crate::error::InputError::TooLarge {
                    limit: crate::utf16::EXCEL_STRING_LIMIT,
                    actual: length,
                },
            ));
        }
    }
    Ok(TopicMetadata {
        byte_len: total_bytes,
        hash: hasher.finish(),
    })
}

/// A validated lookup input; it retains no owned topic storage.
pub(crate) struct BorrowedTopicParts<'a, Part: AsRef<str> = &'a str> {
    parts: &'a [Part],
    metadata: TopicMetadata,
}

impl<'a, Part: AsRef<str>> BorrowedTopicParts<'a, Part> {
    pub(crate) fn new(parts: &'a [Part]) -> XllResult<Self> {
        let metadata = validate_topic_parts(parts.iter().map(AsRef::as_ref))?;
        Ok(Self { parts, metadata })
    }

    pub(super) fn identity_key(&self, source_id: SourceId) -> SubscriptionIdentityKey {
        SubscriptionIdentityKey {
            source_id,
            topic_hash: self.metadata.hash,
        }
    }

    pub(super) fn matches(&self, canonical: &RtdTopic) -> bool {
        self.metadata.hash == canonical.hash
            && self.parts.len() == canonical.parts.len()
            && canonical
                .parts
                .iter()
                .map(SmolStr::as_str)
                .eq(self.parts.iter().map(AsRef::as_ref))
    }

    pub(super) fn into_owned(self) -> RtdTopic {
        RtdTopic {
            parts: self
                .parts
                .iter()
                .map(|part| SmolStr::new(part.as_ref()))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            byte_len: self.metadata.byte_len,
            hash: self.metadata.hash,
        }
    }

    #[cfg(test)]
    pub(super) fn with_test_identity_hash(mut self, hash: u64) -> Self {
        self.metadata.hash = hash;
        self
    }
}

#[cfg(test)]
mod borrowed_tests {
    use super::input::Input;
    use super::*;

    #[test]
    fn topic_input_borrows_validated_storage_and_preserves_cached_metadata() {
        let topic = RtdTopic::new(["market", "USD\0JPY"])
            .unwrap()
            .with_test_identity_hash(42);
        let borrowed = topic.prepare_parts().unwrap();
        assert_eq!(borrowed.parts.as_ptr(), topic.parts.as_ptr());
        assert_eq!(borrowed.metadata.byte_len, topic.byte_len());
        assert_eq!(borrowed.metadata.hash, 42);
        assert!(borrowed.matches(&topic));
    }

    #[test]
    fn topic_input_collections_preserve_original_storage_and_canonical_identity() {
        let canonical = RtdTopic::new(["market", "USD\0JPY"]).unwrap();
        let parts = [String::from("market"), String::from("USD\0JPY")];
        let borrowed = parts.prepare_parts().unwrap();
        assert_eq!(borrowed.parts.as_ptr(), parts.as_ptr());
        assert!(borrowed.matches(&canonical));
        let parts = parts.to_vec();
        let borrowed = parts.prepare_parts().unwrap();
        assert_eq!(borrowed.parts.as_ptr(), parts.as_ptr());
        assert!(borrowed.matches(&canonical));
        assert!(
            parts
                .as_slice()
                .prepare_parts()
                .unwrap()
                .matches(&canonical)
        );
        let references = [&parts[0], &parts[1]];
        assert!(references.prepare_parts().unwrap().matches(&canonical));
    }

    fn check<const N: usize>(parts: [&str; N]) {
        let owned = RtdTopic::new(parts);
        let borrowed = BorrowedTopicParts::new(&parts);
        match (owned, borrowed) {
            (Ok(owned), Ok(borrowed)) => {
                assert!(borrowed.matches(&owned));
                assert_eq!(borrowed.metadata.byte_len, owned.byte_len());
                // Independently verify the canonical hash recipe.
                let mut hasher = FxHasher::default();
                N.hash(&mut hasher);
                for part in parts {
                    part.hash(&mut hasher);
                }
                assert_eq!(borrowed.metadata.hash, hasher.finish());
                assert_eq!(borrowed.into_owned(), owned);
            }
            (Err(owned), Err(borrowed)) => {
                assert_eq!(format!("{owned:?}"), format!("{borrowed:?}"))
            }
            _ => panic!("owning and borrowed validation disagree"),
        }
    }

    #[test]
    fn borrowed_validation_matches_owned_boundaries_and_precedence() {
        check([]);
        check([""]);
        check(["x"; 253]);
        check(["x"; 254]);
        let mut many = ["x"; 254];
        many[0] = "";
        check(many);
        many[0] = "x";
        many[253] = "";
        check(many);
        for length in [32767, 32768] {
            check([&"a".repeat(length)]);
        }
        check([&format!("{}a", "😀".repeat(16383))]);
        check([&"😀".repeat(16384)]);
        check(["market", "USD\0JPY"]);
        let large = "a".repeat(32767);
        let last = "a".repeat(32);
        let mut exact = [large.as_str(); 33];
        exact[32] = &last;
        check(exact);
        let over = "a".repeat(33);
        exact[32] = &over;
        check(exact);
        // Total byte rejection precedes UTF-16 rejection for both routes.
        check([&"a".repeat(MAX_RTD_TOPIC_BYTES + 1)]);
    }

    #[test]
    fn borrowed_utf16_byte_bound_agrees_with_full_unicode_count() {
        for scalar in ["a", "é", "漢", "😀"] {
            for count in [8191, 8192, 10922, 10923, 16383, 16384, 32767, 32768] {
                let text = scalar.repeat(count);
                let expected = text.encode_utf16().count() <= crate::utf16::EXCEL_STRING_LIMIT;
                assert_eq!(BorrowedTopicParts::new(&[text.as_str()]).is_ok(), expected);
                assert_eq!(RtdTopic::new([text.as_str()]).is_ok(), expected);
            }
        }
    }

    #[test]
    fn borrowed_collision_equality_preserves_parts_and_materialized_metadata() {
        let canonical = RtdTopic::new(["ab", "c\0d"])
            .unwrap()
            .with_test_identity_hash(42);
        let same = BorrowedTopicParts::new(&["ab", "c\0d"])
            .unwrap()
            .with_test_identity_hash(42);
        assert!(same.matches(&canonical));
        assert_eq!(
            same.into_owned().identity_hash(),
            42,
            "materialize must not rehash"
        );
        for parts in [["a", "bc\0d"], ["ab", "c\0e"]] {
            assert!(
                !BorrowedTopicParts::new(&parts)
                    .unwrap()
                    .with_test_identity_hash(42)
                    .matches(&canonical)
            );
        }
        assert!(
            !BorrowedTopicParts::new(&["abc\0d"])
                .unwrap()
                .with_test_identity_hash(42)
                .matches(&canonical)
        );
    }
}
