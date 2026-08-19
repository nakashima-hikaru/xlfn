use super::*;
use rustc_hash::FxHashMap;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SourceAddress(usize);

impl SourceAddress {
    pub(crate) fn of<S: ?Sized>(source: &Arc<S>) -> Self {
        let ptr = Arc::as_ptr(source).cast::<()>();
        Self(ptr as usize)
    }
}

pub(crate) struct SourceIdentityEntry {
    pub(crate) id: u64,
    pub(crate) anchor: Weak<dyn ErasedRtdSource>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ResolvedSourceIdentity {
    pub(crate) address: SourceAddress,
    pub(crate) id: u64,
    pub(crate) newly_registered: bool,
}

pub(crate) struct SourceIdentityRegistry {
    pub(crate) by_address: FxHashMap<SourceAddress, SourceIdentityEntry>,
    pub(crate) next_id: u64,
}

impl SourceIdentityRegistry {
    pub(crate) fn new() -> Self {
        Self {
            by_address: FxHashMap::default(),
            next_id: 1,
        }
    }

    pub(crate) fn allocate_id(&mut self) -> XllResult<u64> {
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(XllError::Internal {
            diagnostic_id: crate::DiagnosticId::RTD_SUBSCRIPTION_ID_OVERFLOW,
        })?;
        Ok(id)
    }

    pub(crate) fn resolve<S>(
        &mut self,
        source: &Arc<S>,
        limit: usize,
        downgrade: impl FnOnce() -> Weak<dyn ErasedRtdSource>,
    ) -> XllResult<ResolvedSourceIdentity>
    where
        S: ?Sized,
    {
        let address = SourceAddress::of(source);

        if let Some(entry) = self.by_address.get(&address)
            && entry.anchor.upgrade().is_some()
        {
            return Ok(ResolvedSourceIdentity {
                address,
                id: entry.id,
                newly_registered: false,
            });
        }

        self.by_address.remove(&address);

        if self.by_address.len() >= limit {
            self.reclaim_dead();
        }

        if self.by_address.len() >= limit {
            return Err(XllError::Overloaded);
        }

        let id = self.allocate_id()?;
        let anchor = downgrade();

        self.by_address
            .insert(address, SourceIdentityEntry { id, anchor });

        Ok(ResolvedSourceIdentity {
            address,
            id,
            newly_registered: true,
        })
    }

    pub(crate) fn rollback_registration(&mut self, identity: ResolvedSourceIdentity) {
        if !identity.newly_registered {
            return;
        }

        let should_remove = self
            .by_address
            .get(&identity.address)
            .is_some_and(|entry| entry.id == identity.id);

        if should_remove {
            self.by_address.remove(&identity.address);
        }
    }

    pub(crate) fn reclaim_dead(&mut self) {
        self.by_address
            .retain(|_, entry| entry.anchor.upgrade().is_some());
    }

    pub(crate) fn clear(&mut self) {
        self.by_address.clear();
    }
}

pub(crate) static NEXT_RTD_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn allocate_runtime_id() -> XllResult<u64> {
    NEXT_RTD_RUNTIME_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| XllError::Internal {
            diagnostic_id: crate::DiagnosticId::RTD_RT_ID_OVERFLOW,
        })
}

#[derive(Default)]
pub(crate) struct SubscriptionIdentityIndex {
    pub(crate) key_by_identity: HashMap<SubscriptionIdentity, SubscriptionKey>,
    pub(crate) identity_by_key: HashMap<SubscriptionKey, SubscriptionIdentity>,
}

impl SubscriptionIdentityIndex {
    pub(crate) fn get_key(&self, identity: &SubscriptionIdentity) -> Option<&SubscriptionKey> {
        self.key_by_identity.get(identity)
    }

    pub(crate) fn insert(
        &mut self,
        identity: SubscriptionIdentity,
        key: SubscriptionKey,
    ) -> XllResult<()> {
        if self.key_by_identity.contains_key(&identity) || self.identity_by_key.contains_key(&key) {
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::RTD_INDEX_DUPLICATE,
            });
        }

        self.key_by_identity.insert(identity.clone(), key.clone());
        self.identity_by_key.insert(key, identity);

        Ok(())
    }

    pub(crate) fn remove_by_key(&mut self, key: &SubscriptionKey) -> Option<SubscriptionIdentity> {
        let identity = self.identity_by_key.remove(key)?;
        let removed_key = self.key_by_identity.remove(&identity);
        debug_assert_eq!(removed_key.as_ref(), Some(key));
        Some(identity)
    }

    pub(crate) fn clear(&mut self) {
        self.key_by_identity.clear();
        self.identity_by_key.clear();
    }
}
