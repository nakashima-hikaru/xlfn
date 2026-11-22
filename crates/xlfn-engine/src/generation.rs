use std::num::NonZeroU64;

/// Identity of one published or staged runtime service generation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RuntimeGeneration(NonZeroU64);

impl RuntimeGeneration {
    pub const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Identity of an in-flight open transaction. It is distinct from the
/// published generation even though both participate in lifecycle state.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OpenAttemptId(NonZeroU64);

impl OpenAttemptId {
    pub const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// Promote the identity of a successful open transaction to the
    /// published-generation identity. This is the only conversion between
    /// the two lifecycle domains; callers must not pass either identity
    /// through an untyped integer at the promotion boundary.
    pub const fn into_runtime_generation(self) -> RuntimeGeneration {
        RuntimeGeneration(self.0)
    }
}

/// Monotonic close epoch used to invalidate open attempts sampled before a
/// final removal acquired its owner.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RemovalEpoch(u64);

impl RemovalEpoch {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Identity of one owner of the terminal removal protocol.
///
/// A removal request may advance the close epoch more than once while callers
/// wait for an earlier owner to leave. This identity is therefore separate
/// from [`RemovalEpoch`]: it names the affine owner that is allowed to issue a
/// terminal certificate.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RemovalAttemptId(NonZeroU64);

impl RemovalAttemptId {
    pub const ONE: Self = Self(NonZeroU64::MIN);

    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    pub const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }
}

#[cfg(test)]
mod removal_attempt_tests {
    use super::RemovalAttemptId;

    #[test]
    fn exhaustion_never_wraps_to_an_old_removal_identity() {
        let penultimate = RemovalAttemptId::new(u64::MAX - 1).unwrap();
        let last = penultimate.checked_next().unwrap();
        assert_eq!(last, RemovalAttemptId::new(u64::MAX).unwrap());
        assert!(last.checked_next().is_none());
        assert_ne!(last, RemovalAttemptId::ONE);
    }
}

/// Identity of one COM/RTD server instance. The zero sentinel is reserved for
/// the absence of an active server and never enters subscription maps.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ServerGeneration(NonZeroU64);

impl ServerGeneration {
    pub const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Monotonic identity of one subscription connection attempt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ConnectionGeneration(NonZeroU64);

impl ConnectionGeneration {
    pub const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }
}
