//! Runtime-owned generation service slots.

use crate::generation::RuntimeGeneration;

/// Service slots whose liveness is coupled to one open generation.
/// Generation-specific policy is consumed from [`crate::runtime::OpeningGeneration`]
/// while these slots carry the service state owned by the open bundle.
pub(crate) struct GenerationServices {
    formula_handles: crate::handle::FormulaHandleServiceSlot,
    subscriptions: crate::rtd::SubscriptionServiceSlot,
    subscription_host: crate::rtd::RtdSubscriptionHost,
}

/// Linear teardown token for the generation-scoped service bundle.
///
/// Subscription shutdown happens in the producer-drain stage, before handle
/// sealing. This token reunites that subscription certificate with the handle
/// slot seal so the two generation services leave the runtime as one unit.
pub(crate) struct SealedGenerationServices {
    formula_handles: crate::handle::FormulaHandleServiceSealed,
    subscriptions_stopped: crate::rtd::SubscriptionsStopped,
}

/// Runtime-owned executors whose lifecycle is independent from generation
/// service arming.
#[cfg(feature = "async")]
pub(crate) struct RuntimeExecutors {
    pub(crate) async_manager: crate::async_udf::AsyncManager,
}

/// Reservation for a generation-owned service bundle.
///
/// Arming is a transaction: until the reservation is committed, dropping it
/// rolls both slots back. A rollback failure means the slot protocol has
/// violated its ownership invariant, so it is fail-stopped instead of being
/// silently discarded at an open boundary.
#[must_use = "an armed service reservation must be committed or rolled back"]
pub(crate) struct ArmedServices {
    services: Option<GenerationServices>,
    committed: bool,
}

impl ArmedServices {
    pub(crate) fn commit(mut self) -> std::sync::Arc<GenerationServices> {
        self.committed = true;
        std::sync::Arc::new(
            self.services
                .take()
                .expect("an armed service reservation owns its service bundle"),
        )
    }
}

impl Drop for ArmedServices {
    fn drop(&mut self) {
        if !self.committed
            && let Some(services) = self.services.as_ref()
        {
            services.disarm_or_abort();
        }
    }
}

impl GenerationServices {
    #[cfg(test)]
    pub(crate) const fn new() -> Self {
        Self {
            formula_handles: crate::handle::FormulaHandleServiceSlot::new(),
            subscriptions: crate::rtd::SubscriptionServiceSlot::new(),
            subscription_host: crate::rtd::RtdSubscriptionHost::detached(),
        }
    }

    pub(crate) fn arm_generation(
        generation: RuntimeGeneration,
        config: crate::addin::RuntimeConfig,
        subscription_host: crate::rtd::RtdSubscriptionHost,
    ) -> crate::XllResult<ArmedServices> {
        let services = Self {
            formula_handles: crate::handle::FormulaHandleServiceSlot::new(),
            subscriptions: crate::rtd::SubscriptionServiceSlot::new(),
            subscription_host,
        };
        services.formula_handles.arm(config.handle_config())?;
        if let Err(error) = services.subscriptions.arm(generation, config.rtd_limits()) {
            services.disarm_or_abort();
            return Err(error);
        }
        if let Err(error) = services.formula_handles.initialize() {
            services.disarm_or_abort();
            return Err(error);
        }
        Ok(ArmedServices {
            services: Some(services),
            committed: false,
        })
    }

    pub(crate) fn formula_handle_slot(&self) -> &crate::handle::FormulaHandleServiceSlot {
        &self.formula_handles
    }

    pub(crate) fn subscriptions_slot(&self) -> &crate::rtd::SubscriptionServiceSlot {
        &self.subscriptions
    }

    pub(crate) fn subscription_host(&self) -> crate::rtd::RtdSubscriptionHost {
        self.subscription_host
    }

    /// Stop the RTD producer associated with this generation without making
    /// the formula-handle service depend on the RTD adapter.
    pub(crate) fn shutdown_handle_topics(&self) -> crate::XllResult<()> {
        let Some(handles) = self.formula_handles.read_if_ready() else {
            return Ok(());
        };
        crate::rtd::shutdown_handle_topics(std::sync::Arc::clone(handles.as_arc()))
    }

    pub(crate) fn seal(
        &self,
        generation: Option<RuntimeGeneration>,
        subscriptions_stopped: crate::rtd::SubscriptionsStopped,
    ) -> crate::XllResult<SealedGenerationServices> {
        let formula_handles = self.formula_handles.seal(generation)?;
        Ok(SealedGenerationServices {
            formula_handles,
            subscriptions_stopped,
        })
    }

    pub(crate) fn is_none(&self) -> bool {
        self.formula_handles.is_none() && self.subscriptions.is_none()
    }

    pub(crate) fn disarm_or_abort(&self) {
        if let Err(error) = self.disarm_generation() {
            tracing::error!(%error, "generation service rollback violated its state invariant");
            std::process::abort();
        }
    }

    pub(crate) fn disarm_generation(&self) -> crate::XllResult<()> {
        let handle_result = self.formula_handles.disarm();
        let subscription_result = self.subscriptions.disarm();
        handle_result.and(subscription_result)
    }
}

impl SealedGenerationServices {
    pub(crate) fn empty(
        generation: Option<RuntimeGeneration>,
        subscriptions_stopped: crate::rtd::SubscriptionsStopped,
    ) -> Self {
        Self {
            formula_handles: crate::handle::FormulaHandleServiceSealed::empty(generation),
            subscriptions_stopped,
        }
    }

    pub(crate) fn finish(
        self,
    ) -> crate::XllResult<(
        crate::shutdown::HandleStoreQuiescent,
        crate::rtd::SubscriptionsStopped,
    )> {
        let handle_store_quiescent = self.formula_handles.finish()?;
        Ok((handle_store_quiescent, self.subscriptions_stopped))
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
