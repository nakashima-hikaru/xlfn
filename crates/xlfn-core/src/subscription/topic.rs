use super::*;

pub(crate) const DEFAULT_MAX_RTD_TOPIC_PARTS: usize = 253;
pub(crate) const DEFAULT_MAX_RTD_TOPIC_BYTES: usize = 1024 * 1024;
pub(crate) const DEFAULT_MAX_RTD_PENDING: usize = 4096;
pub(crate) const DEFAULT_MAX_RTD_ACTIVE: usize = 4096;
pub(crate) const DEFAULT_MAX_RTD_QUEUED_UPDATES: usize = 4096;
pub(crate) const DEFAULT_MAX_RTD_SOURCE_IDS: usize = 4096;
pub(crate) const DEFAULT_MAX_RTD_TOTAL_TOPIC_BYTES: usize = 64 * 1024 * 1024;

/// Resource limits for one add-in's RTD subscription runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtdLimits {
    pub max_topic_parts: usize,
    pub max_topic_bytes: usize,
    pub max_pending: usize,
    pub max_active: usize,
    pub max_queued_updates: usize,
    pub max_source_ids: usize,
    pub max_total_topic_bytes: usize,
}

impl RtdLimits {
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            max_topic_parts: DEFAULT_MAX_RTD_TOPIC_PARTS,
            max_topic_bytes: DEFAULT_MAX_RTD_TOPIC_BYTES,
            max_pending: DEFAULT_MAX_RTD_PENDING,
            max_active: DEFAULT_MAX_RTD_ACTIVE,
            max_queued_updates: DEFAULT_MAX_RTD_QUEUED_UPDATES,
            max_source_ids: DEFAULT_MAX_RTD_SOURCE_IDS,
            max_total_topic_bytes: DEFAULT_MAX_RTD_TOTAL_TOPIC_BYTES,
        }
    }
}

impl Default for RtdLimits {
    fn default() -> Self {
        Self::standard()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ServerGeneration(pub(crate) u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct TopicId(pub(crate) i32);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SourceId(pub(crate) u64);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SubscriptionIdentity {
    pub(crate) source_id: SourceId,
    pub(crate) topic: RtdTopic,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SubscriptionKey(pub(crate) Arc<str>);

impl SubscriptionKey {
    pub(crate) fn from_allocated_id(runtime_id: u64, subscription_id: u64) -> Self {
        Self(format!("stream:v1:{runtime_id:016x}:{subscription_id:016x}").into())
    }

    pub(crate) fn parse_transport(value: &str) -> XllResult<Self> {
        const PREFIX: &str = "stream:v1:";

        let Some(rest) = value.strip_prefix(PREFIX) else {
            return Err(XllError::InvalidHandle);
        };

        let Some((runtime, subscription)) = rest.split_once(':') else {
            return Err(XllError::InvalidHandle);
        };

        if runtime.len() != 16
            || subscription.len() != 16
            || !runtime.as_bytes().iter().all(|b| b.is_ascii_hexdigit())
            || !subscription
                .as_bytes()
                .iter()
                .all(|b| b.is_ascii_hexdigit())
        {
            return Err(XllError::InvalidHandle);
        }

        Ok(Self(value.into()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for SubscriptionKey {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PartialEq<str> for SubscriptionKey {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for SubscriptionKey {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<SubscriptionKey> for str {
    fn eq(&self, other: &SubscriptionKey) -> bool {
        self == other.as_str()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ConnectionGeneration(pub(crate) u64);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RtdTopic {
    pub(crate) parts: Arc<[String]>,
}

impl RtdTopic {
    pub fn new(parts: impl IntoIterator<Item = impl Into<String>>) -> XllResult<Self> {
        let limits = RtdLimits::standard();
        let mut normalized = Vec::new();
        let mut total_bytes = 0_usize;
        for part in parts {
            if normalized.len() >= limits.max_topic_parts {
                return Err(XllError::input(
                    "RTD topic",
                    crate::InputError::TooLarge {
                        limit: limits.max_topic_parts,
                        actual: normalized.len().saturating_add(1),
                    },
                ));
            }
            let part = part.into();
            if part.is_empty() {
                return Err(XllError::input(
                    "RTD topic",
                    crate::InputError::Malformed("RTD topics require non-empty parts"),
                ));
            }
            total_bytes = total_bytes.checked_add(part.len()).ok_or_else(|| {
                XllError::input(
                    "RTD topic",
                    crate::InputError::TooLarge {
                        limit: limits.max_topic_bytes,
                        actual: usize::MAX,
                    },
                )
            })?;
            if total_bytes > limits.max_topic_bytes {
                return Err(XllError::input(
                    "RTD topic",
                    crate::InputError::TooLarge {
                        limit: limits.max_topic_bytes,
                        actual: total_bytes,
                    },
                ));
            }
            normalized.push(part);
        }
        if normalized.is_empty() {
            return Err(XllError::input(
                "RTD topic",
                crate::InputError::Malformed("RTD topics require non-empty parts"),
            ));
        }
        Ok(Self {
            parts: Arc::from(normalized),
        })
    }

    pub fn single(part: impl Into<String>) -> XllResult<Self> {
        Self::new([part.into()])
    }

    #[must_use]
    pub fn parts(&self) -> &[String] {
        &self.parts
    }

    pub(crate) fn validate_with_limits(&self, limits: &RtdLimits) -> XllResult<()> {
        validate_topic_parts(&self.parts, limits)
    }

    pub(crate) fn byte_len(&self) -> usize {
        self.parts.iter().map(String::len).sum()
    }
}

fn validate_topic_parts(parts: &[String], limits: &RtdLimits) -> XllResult<()> {
    if parts.is_empty() || parts.iter().any(String::is_empty) {
        return Err(XllError::input(
            "RTD topic",
            crate::InputError::Malformed("RTD topics require non-empty parts"),
        ));
    }
    if parts.len() > limits.max_topic_parts {
        return Err(XllError::input(
            "RTD topic",
            crate::InputError::TooLarge {
                limit: limits.max_topic_parts,
                actual: parts.len(),
            },
        ));
    }

    let mut total_bytes = 0_usize;
    for part in parts {
        let utf16_len = part.encode_utf16().count();
        if utf16_len > 32_767 {
            return Err(XllError::input(
                "RTD topic",
                crate::InputError::TooLarge {
                    limit: 32_767,
                    actual: utf16_len,
                },
            ));
        }
        total_bytes = total_bytes.checked_add(part.len()).ok_or_else(|| {
            XllError::input(
                "RTD topic",
                crate::InputError::TooLarge {
                    limit: limits.max_topic_bytes,
                    actual: usize::MAX,
                },
            )
        })?;
    }
    if total_bytes > limits.max_topic_bytes {
        return Err(XllError::input(
            "RTD topic",
            crate::InputError::TooLarge {
                limit: limits.max_topic_bytes,
                actual: total_bytes,
            },
        ));
    }
    Ok(())
}
