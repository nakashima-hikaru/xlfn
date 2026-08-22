use super::identity::{SourceIdentityRegistry, SubscriptionIdentityIndex};
use super::source::ErasedRtdSource;
use super::topic::{RtdTopic, SubscriptionKey};
use crate::generation::{ConnectionGeneration, ServerGeneration};
use crate::{XllError, XllResult};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BindingStage {
    Connecting,
    Active,
}

pub(crate) struct ActiveKeyBinding {
    pub(crate) connection_generation: ConnectionGeneration,
    pub(crate) stage: BindingStage,
}

pub(crate) struct PendingSubscription {
    pub(crate) live_reservations: usize,
    pub(crate) committed: bool,
    pub(crate) source: Arc<dyn ErasedRtdSource>,
    pub(crate) topic: RtdTopic,
    pub(crate) server_generation: Option<ServerGeneration>,
    pub(crate) connecting_generation: Option<ConnectionGeneration>,
}

pub(crate) struct SubscriptionCatalog {
    pub(crate) pending: HashMap<SubscriptionKey, PendingSubscription>,
    pub(crate) pending_topic_bytes: usize,
    pub(crate) sources: SourceIdentityRegistry,
    pub(crate) active_keys: HashMap<SubscriptionKey, ActiveKeyBinding>,
    pub(crate) identities: SubscriptionIdentityIndex,
    pub(crate) next_subscription_id: u64,
}

impl SubscriptionCatalog {
    pub(crate) fn allocate_key(&mut self, runtime_id: u64) -> XllResult<SubscriptionKey> {
        let id = self.next_subscription_id;
        self.next_subscription_id = id.checked_add(1).ok_or(XllError::Internal {
            diagnostic_id: crate::error::DiagnosticId::RTD_SUBSCRIPTION_OVERFLOW,
        })?;
        Ok(SubscriptionKey::from_allocated_id(runtime_id, id))
    }

    #[cfg(test)]
    pub(crate) fn assert_identity_invariants(&self) {
        assert_eq!(
            self.identities.key_by_identity.len(),
            self.identities.identity_by_key.len(),
        );

        for (identity, key) in &self.identities.key_by_identity {
            assert_eq!(self.identities.identity_by_key.get(key), Some(identity),);

            assert!(self.pending.contains_key(key) || self.active_keys.contains_key(key),);
        }
    }
}

pub(crate) fn remove_identity_if_unbound(catalog: &mut SubscriptionCatalog, key: &SubscriptionKey) {
    let has_pending = catalog.pending.contains_key(key);
    let has_active = catalog.active_keys.contains_key(key);

    if !has_pending && !has_active {
        let _ = catalog.identities.remove_by_key(key);
    }
}
