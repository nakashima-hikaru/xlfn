//! Private ownership components for [`crate::Runtime`].
//!
//! These types are intentionally crate-private. They make the protocol
//! membership of the runtime state explicit without creating new public crate
//! boundaries or exposing lifecycle bookkeeping to add-in authors.

use arc_swap::ArcSwapOption;
use parking_lot::{Condvar, Mutex, RwLock};
use std::collections::BTreeMap;
#[cfg(any(test, feature = "shutdown-refinement"))]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64};

use crate::module_residency::ModuleResidencyLease;
use crate::registration::{EventRegistration, ExcelNameKey, MetadataDebt, PendingRegistration};
use crate::runtime::{HostLifecycleIntent, LifecyclePhase, OpenGeneration, OpeningGeneration};

/// Lifecycle synchronization state: phase, epochs, open ownership, and the
/// published generation all move together under the lifecycle protocol.
pub(crate) struct LifecycleState<A: crate::Addin> {
    pub(crate) phase: AtomicU8,
    pub(crate) host_intent: AtomicU8,
    pub(crate) next_lifecycle_attempt: AtomicU64,
    pub(crate) generation: AtomicU64,
    pub(crate) open_attempt_id: AtomicU64,
    pub(crate) removal_epoch: AtomicU64,
    pub(crate) opening: Mutex<Option<OpeningGeneration<A>>>,
    pub(crate) current: ArcSwapOption<OpenGeneration<A>>,
    pub(crate) wait_lock: Mutex<()>,
    pub(crate) lifecycle_changed: Condvar,
    pub(crate) removal_attempt_active: AtomicBool,
    #[cfg(test)]
    pub(crate) test_module_lease: Mutex<Option<crate::ingress::TestModuleLease>>,
}

impl<A: crate::Addin> LifecycleState<A> {
    pub(crate) const fn new() -> Self {
        Self {
            phase: AtomicU8::new(LifecyclePhase::Closed as u8),
            host_intent: AtomicU8::new(HostLifecycleIntent::None as u8),
            next_lifecycle_attempt: AtomicU64::new(1),
            generation: AtomicU64::new(0),
            open_attempt_id: AtomicU64::new(0),
            removal_epoch: AtomicU64::new(0),
            opening: Mutex::new(None),
            current: ArcSwapOption::const_empty(),
            wait_lock: Mutex::new(()),
            lifecycle_changed: Condvar::new(),
            removal_attempt_active: AtomicBool::new(false),
            #[cfg(test)]
            test_module_lease: Mutex::new(None),
        }
    }
}

/// The Excel host registration protocol and its recovery ledger.
pub(crate) struct HostLedger {
    pub(crate) registrations: Mutex<Vec<PendingRegistration>>,
    pub(crate) metadata_debt: Mutex<BTreeMap<ExcelNameKey, Vec<MetadataDebt>>>,
    pub(crate) event_registrations: Mutex<Vec<EventRegistration>>,
    pub(crate) registration_state_unknown: AtomicBool,
}

impl HostLedger {
    pub(crate) const fn new() -> Self {
        Self {
            registrations: Mutex::new(Vec::new()),
            metadata_debt: Mutex::new(BTreeMap::new()),
            event_registrations: Mutex::new(Vec::new()),
            registration_state_unknown: AtomicBool::new(false),
        }
    }
}

/// Excel return ownership and call/calculation identity state.
pub(crate) struct ReturnProtocol {
    pub(crate) returns: crate::return_value::ReturnTracker,
    pub(crate) next_call_id: AtomicU64,
    #[cfg(not(feature = "async"))]
    pub(crate) calculation_id: AtomicU64,
}

impl ReturnProtocol {
    pub(crate) const fn new() -> Self {
        Self {
            returns: crate::return_value::ReturnTracker::new_closed(),
            next_call_id: AtomicU64::new(1),
            #[cfg(not(feature = "async"))]
            calculation_id: AtomicU64::new(1),
        }
    }
}

/// Framework services owned by one open generation.
pub(crate) struct RuntimeServices {
    pub(crate) handles: crate::handle::HandleRuntimeSlot,
    pub(crate) subscriptions: crate::subscription::SubscriptionRuntimeSlot,
    pub(crate) rtd_limits: RwLock<crate::subscription::RtdLimits>,
    #[cfg(feature = "async")]
    pub(crate) async_manager: crate::async_udf::AsyncManager,
}

impl RuntimeServices {
    pub(crate) const fn new(rtd_limits: crate::subscription::RtdLimits) -> Self {
        Self {
            handles: crate::handle::HandleRuntimeSlot::new(),
            subscriptions: crate::subscription::SubscriptionRuntimeSlot::new(),
            rtd_limits: RwLock::new(rtd_limits),
            #[cfg(feature = "async")]
            async_manager: crate::async_udf::AsyncManager::new(),
        }
    }
}

/// Physical DLL residency is deliberately separate from logical lifecycle
/// state. A quarantined or logically closed runtime can retain this lease.
pub(crate) struct ModuleResidency {
    pub(crate) lease: Mutex<Option<ModuleResidencyLease>>,
}

impl ModuleResidency {
    pub(crate) const fn new() -> Self {
        Self {
            lease: Mutex::new(None),
        }
    }
}

#[cfg(any(test, feature = "shutdown-refinement"))]
/// Verification-only state is isolated from operational runtime components.
pub(crate) struct FormalState {
    pub(crate) ghost: std::sync::OnceLock<crate::shutdown_refinement::GhostHandle>,
    pub(crate) composition:
        std::sync::OnceLock<Arc<crate::composition_refinement::CompositionTrace>>,
}

#[cfg(any(test, feature = "shutdown-refinement"))]
impl FormalState {
    pub(crate) const fn new() -> Self {
        Self {
            ghost: std::sync::OnceLock::new(),
            composition: std::sync::OnceLock::new(),
        }
    }
}
