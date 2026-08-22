//! Runtime-owned generation service slots.

use crate::generation::RuntimeGeneration;

/// Service slots whose liveness is coupled to one open generation.
/// Generation-specific policy lives in [`crate::addin::RuntimeConfig`] inside
/// [`crate::runtime::OpenGeneration`], while these slots carry the reusable
/// service state.
pub(crate) struct GenerationServices {
    pub(crate) handles: crate::handle::HandleRuntimeSlot,
    pub(crate) subscriptions: crate::subscription::slot::SubscriptionRuntimeSlot,
}

/// Runtime-owned executors whose lifecycle is independent from generation
/// service arming.
#[cfg(feature = "async")]
pub(crate) struct RuntimeExecutors {
    pub(crate) async_manager: crate::async_udf::AsyncManager,
}

/// Reservation for the two generation-scoped service slots.
///
/// Arming is a transaction: until the reservation is committed, dropping it
/// rolls both slots back. A rollback failure means the slot protocol has
/// violated its ownership invariant, so it is fail-stopped instead of being
/// silently discarded at an open boundary.
#[must_use = "an armed service reservation must be committed or rolled back"]
pub(crate) struct ArmedServices<'a> {
    services: &'a GenerationServices,
    generation: RuntimeGeneration,
    committed: bool,
}

impl ArmedServices<'_> {
    pub(crate) fn commit(mut self) {
        self.committed = true;
    }

    pub(crate) fn rollback(mut self) {
        self.committed = true;
        self.services.disarm_or_abort(self.generation);
    }
}

impl Drop for ArmedServices<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.services.disarm_or_abort(self.generation);
        }
    }
}

impl GenerationServices {
    pub(crate) const fn new() -> Self {
        Self {
            handles: crate::handle::HandleRuntimeSlot::new(),
            subscriptions: crate::subscription::slot::SubscriptionRuntimeSlot::new(),
        }
    }

    pub(crate) fn arm_generation(
        &self,
        generation: RuntimeGeneration,
        config: crate::addin::RuntimeConfig,
    ) -> crate::XllResult<ArmedServices<'_>> {
        self.handles.arm(generation, config.handle_config())?;
        let armed = ArmedServices {
            services: self,
            generation,
            committed: false,
        };
        self.subscriptions
            .arm(generation, config.rtd_limits())
            .map(|()| armed)
    }

    fn disarm_or_abort(&self, generation: RuntimeGeneration) {
        if let Err(error) = self.disarm_generation(generation) {
            tracing::error!(
                ?generation,
                %error,
                "generation service rollback violated its state invariant"
            );
            std::process::abort();
        }
    }

    pub(crate) fn disarm_generation(&self, generation: RuntimeGeneration) -> crate::XllResult<()> {
        let handle_result = self.handles.disarm(generation);
        let subscription_result = self.subscriptions.disarm(generation);
        handle_result.and(subscription_result)
    }
}

#[cfg(feature = "async")]
impl RuntimeExecutors {
    pub(crate) const fn new() -> Self {
        Self {
            async_manager: crate::async_udf::AsyncManager::new(),
        }
    }
}
