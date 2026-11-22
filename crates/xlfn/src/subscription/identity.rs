use super::topic::{SubscriptionId, SubscriptionIdentityKey};
use crate::{XllError, XllResult};
use std::sync::atomic::{AtomicU64, Ordering};
use xlfn_services::rtd::index::{IdentityIndex, IndexError};

pub(crate) type SubscriptionIdentityIndex = IdentityIndex<SubscriptionIdentityKey, SubscriptionId>;

pub(crate) fn map_index_error(error: IndexError) -> XllError {
    let diagnostic_id = match error {
        IndexError::Duplicate => crate::diagnostics::catalog::RTD_INDEX_DUPLICATE,
        IndexError::Overflow => crate::diagnostics::catalog::RTD_SUBSCRIPTION_OVERFLOW,
        IndexError::SourceLimit => return XllError::Overloaded,
    };
    crate::error::construct::internal(diagnostic_id)
}

pub(crate) static NEXT_RTD_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn allocate_runtime_id() -> XllResult<u64> {
    NEXT_RTD_RUNTIME_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| {
            crate::error::construct::internal(crate::diagnostics::catalog::RTD_RT_ID_OVERFLOW)
        })
}
