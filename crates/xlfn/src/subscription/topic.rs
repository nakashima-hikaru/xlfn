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
#[non_exhaustive]
pub struct RtdLimits {
    pub(crate) max_topic_parts: usize,
    pub(crate) max_topic_bytes: usize,
    pub(crate) max_pending: usize,
    pub(crate) max_active: usize,
    pub(crate) max_queued_updates: usize,
    pub(crate) max_source_ids: usize,
    pub(crate) max_total_topic_bytes: usize,
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

    #[must_use]
    pub const fn with_max_topic_parts(mut self, value: usize) -> Self {
        self.max_topic_parts = value;
        self
    }

    #[must_use]
    pub const fn with_max_topic_bytes(mut self, value: usize) -> Self {
        self.max_topic_bytes = value;
        self
    }

    #[must_use]
    pub const fn with_max_pending(mut self, value: usize) -> Self {
        self.max_pending = value;
        self
    }

    #[must_use]
    pub const fn with_max_active(mut self, value: usize) -> Self {
        self.max_active = value;
        self
    }

    #[must_use]
    pub const fn with_max_queued_updates(mut self, value: usize) -> Self {
        self.max_queued_updates = value;
        self
    }

    #[must_use]
    pub const fn with_max_source_ids(mut self, value: usize) -> Self {
        self.max_source_ids = value;
        self
    }

    #[must_use]
    pub const fn with_max_total_topic_bytes(mut self, value: usize) -> Self {
        self.max_total_topic_bytes = value;
        self
    }

    #[must_use]
    pub const fn max_topic_parts(&self) -> usize {
        self.max_topic_parts
    }

    #[must_use]
    pub const fn max_topic_bytes(&self) -> usize {
        self.max_topic_bytes
    }

    #[must_use]
    pub const fn max_pending(&self) -> usize {
        self.max_pending
    }

    #[must_use]
    pub const fn max_active(&self) -> usize {
        self.max_active
    }

    #[must_use]
    pub const fn max_queued_updates(&self) -> usize {
        self.max_queued_updates
    }

    #[must_use]
    pub const fn max_source_ids(&self) -> usize {
        self.max_source_ids
    }

    #[must_use]
    pub const fn max_total_topic_bytes(&self) -> usize {
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
pub(crate) struct SourceId(pub(crate) u64);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SubscriptionIdentity {
    pub(crate) source_id: SourceId,
    pub(crate) topic: RtdTopic,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SubscriptionKey {
    runtime_id: u64,
    subscription_id: u64,
}

impl SubscriptionKey {
    pub(crate) const fn from_allocated_id(runtime_id: u64, subscription_id: u64) -> Self {
        Self {
            runtime_id,
            subscription_id,
        }
    }

    pub(crate) fn to_transport(self) -> String {
        format!(
            "stream:v1:{:016x}:{:016x}",
            self.runtime_id, self.subscription_id
        )
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
