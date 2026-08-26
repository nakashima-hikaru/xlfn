//! Runtime-owned generation service slots.

use crate::generation::RuntimeGeneration;

/// Service slots whose liveness is coupled to one open generation.
/// Generation-specific policy is consumed from [`crate::generation::OpeningGeneration`]
/// while these slots carry the service state owned by the open bundle.
pub(crate) struct GenerationServices {
    formula_handles: FormulaHandleServices,
    #[cfg(any(feature = "rtd", test))]
    rtd: RtdGenerationServices,
}

enum FormulaHandleServices {
    #[cfg(any(feature = "handles", test))]
    Active(crate::handle::FormulaHandleServiceSlot),
    #[cfg(not(any(feature = "handles", test)))]
    Absent,
}

#[cfg(any(feature = "rtd", test))]
struct RtdGenerationServices {
    subscriptions: crate::excel_rtd::SubscriptionServiceSlot,
    subscription_host: crate::excel_rtd::RtdSubscriptionHost,
}

/// Access to the optional formula-handle capability in one generation.
pub(crate) struct FormulaHandleSlotAccess<'a> {
    #[cfg(any(feature = "handles", test))]
    services: &'a FormulaHandleServices,
    #[cfg(not(any(feature = "handles", test)))]
    _marker: std::marker::PhantomData<&'a ()>,
}

impl FormulaHandleServices {
    const fn new() -> Self {
        #[cfg(any(feature = "handles", test))]
        {
            Self::Active(crate::handle::FormulaHandleServiceSlot::new())
        }
        #[cfg(not(any(feature = "handles", test)))]
        Self::Absent
    }

    #[cfg(any(feature = "handles", test))]
    fn arm(&self, config: crate::addin::HandleConfig) -> crate::XllResult<()> {
        #[cfg(any(feature = "handles", test))]
        {
            let Self::Active(slot) = self;
            slot.arm(config)
        }
    }

    #[cfg(not(any(feature = "handles", test)))]
    fn arm(&self) -> crate::XllResult<()> {
        Ok(())
    }

    fn initialize(&self) -> crate::XllResult<()> {
        #[cfg(any(feature = "handles", test))]
        {
            let Self::Active(slot) = self;
            slot.initialize()
        }
        #[cfg(not(any(feature = "handles", test)))]
        Ok(())
    }

    fn access(&self) -> FormulaHandleSlotAccess<'_> {
        #[cfg(any(feature = "handles", test))]
        {
            FormulaHandleSlotAccess { services: self }
        }
        #[cfg(not(any(feature = "handles", test)))]
        FormulaHandleSlotAccess {
            _marker: std::marker::PhantomData,
        }
    }

    fn seal(
        &self,
        generation: Option<RuntimeGeneration>,
    ) -> crate::XllResult<crate::shutdown::HandlesSealed> {
        #[cfg(any(feature = "handles", test))]
        {
            let Self::Active(slot) = self;
            slot.seal(generation)
        }
        #[cfg(not(any(feature = "handles", test)))]
        Ok(crate::shutdown::HandlesSealed::empty(generation))
    }

    fn is_none(&self) -> bool {
        #[cfg(any(feature = "handles", test))]
        {
            let Self::Active(slot) = self;
            slot.is_none()
        }
        #[cfg(not(any(feature = "handles", test)))]
        true
    }

    fn disarm(&self) -> crate::XllResult<()> {
        #[cfg(any(feature = "handles", test))]
        {
            let Self::Active(slot) = self;
            slot.disarm()
        }
        #[cfg(not(any(feature = "handles", test)))]
        Ok(())
    }
}

impl FormulaHandleSlotAccess<'_> {
    #[cfg(test)]
    pub(crate) fn is_none(&self) -> bool {
        self.services.is_none()
    }

    pub(crate) fn read(&self) -> crate::XllResult<crate::handle::FormulaHandleServiceRead> {
        #[cfg(any(feature = "handles", test))]
        {
            let FormulaHandleServices::Active(slot) = self.services;
            slot.read()
        }
        #[cfg(not(any(feature = "handles", test)))]
        Err(crate::XllError::Closing)
    }

    pub(crate) fn read_if_ready(&self) -> Option<crate::handle::FormulaHandleServiceRead> {
        #[cfg(any(feature = "handles", test))]
        {
            let FormulaHandleServices::Active(slot) = self.services;
            slot.read_if_ready()
        }
        #[cfg(not(any(feature = "handles", test)))]
        None
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn get_owned(
        &self,
    ) -> crate::XllResult<std::sync::Arc<crate::handle::FormulaHandleService>> {
        {
            let FormulaHandleServices::Active(slot) = self.services;
            slot.get_owned()
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        #[cfg(any(feature = "handles", test))]
        {
            let FormulaHandleServices::Active(slot) = self.services;
            slot.set_trace_sink(trace);
        }
        #[cfg(not(any(feature = "handles", test)))]
        let _ = trace;
    }
}

/// Linear teardown token for the generation-scoped service bundle.
///
/// Subscription shutdown happens in the producer-drain stage, before handle
/// sealing. This token reunites that subscription certificate with the handle
/// slot seal so the two generation services leave the runtime as one unit.
pub(crate) struct SealedGenerationServices {
    handles: crate::shutdown::HandlesSealed,
    subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
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
            formula_handles: FormulaHandleServices::new(),
            rtd: RtdGenerationServices {
                subscriptions: crate::excel_rtd::SubscriptionServiceSlot::new(),
                subscription_host: crate::excel_rtd::RtdSubscriptionHost::detached(),
            },
        }
    }

    pub(crate) fn arm_generation(
        generation: RuntimeGeneration,
        _config: crate::addin::RuntimeConfig,
        subscription_host: Option<crate::excel_rtd::RtdSubscriptionHost>,
    ) -> crate::XllResult<ArmedServices> {
        #[cfg(not(any(feature = "rtd", test)))]
        let _ = (generation, subscription_host);
        let services = Self {
            formula_handles: FormulaHandleServices::new(),
            #[cfg(any(feature = "rtd", test))]
            rtd: RtdGenerationServices {
                subscriptions: crate::excel_rtd::SubscriptionServiceSlot::new(),
                subscription_host: subscription_host
                    .expect("RTD generation services require an RTD host capability"),
            },
        };
        #[cfg(any(feature = "handles", test))]
        services.formula_handles.arm(_config.handle_config())?;
        #[cfg(not(any(feature = "handles", test)))]
        services.formula_handles.arm()?;
        #[cfg(any(feature = "rtd", test))]
        if let Err(error) = services
            .rtd
            .subscriptions
            .arm(generation, _config.rtd_limits())
        {
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

    pub(crate) fn formula_handle_slot(&self) -> FormulaHandleSlotAccess<'_> {
        self.formula_handles.access()
    }

    #[cfg(any(feature = "rtd", test))]
    pub(crate) fn subscriptions_slot(&self) -> &crate::excel_rtd::SubscriptionServiceSlot {
        &self.rtd.subscriptions
    }

    #[cfg(any(feature = "rtd", test))]
    pub(crate) fn subscription_host(&self) -> crate::excel_rtd::RtdSubscriptionHost {
        self.rtd.subscription_host
    }

    /// Stop the RTD producer associated with this generation without making
    /// the formula-handle service depend on the RTD adapter.
    pub(crate) fn shutdown_handle_topics(&self) -> crate::XllResult<()> {
        let Some(handles) = self.formula_handles.access().read_if_ready() else {
            return Ok(());
        };
        crate::excel_rtd::shutdown_handle_topics(std::sync::Arc::clone(handles.as_arc()))
    }

    pub(crate) fn seal(
        &self,
        generation: Option<RuntimeGeneration>,
        subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    ) -> crate::XllResult<SealedGenerationServices> {
        let handles = self.formula_handles.seal(generation)?;
        Ok(SealedGenerationServices {
            handles,
            subscriptions_stopped,
        })
    }

    pub(crate) fn is_none(&self) -> bool {
        self.formula_handles.is_none() && {
            #[cfg(any(feature = "rtd", test))]
            {
                self.rtd.subscriptions.is_none()
            }
            #[cfg(not(any(feature = "rtd", test)))]
            {
                true
            }
        }
    }

    pub(crate) fn disarm_or_abort(&self) {
        if let Err(error) = self.disarm_generation() {
            tracing::error!(%error, "generation service rollback violated its state invariant");
            std::process::abort();
        }
    }

    pub(crate) fn disarm_generation(&self) -> crate::XllResult<()> {
        let handle_result = self.formula_handles.disarm();
        #[cfg(any(feature = "rtd", test))]
        let subscription_result = self.rtd.subscriptions.disarm();
        #[cfg(not(any(feature = "rtd", test)))]
        let subscription_result = Ok(());
        handle_result.and(subscription_result)
    }
}

impl SealedGenerationServices {
    pub(crate) fn empty(
        generation: Option<RuntimeGeneration>,
        subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    ) -> Self {
        Self {
            handles: crate::shutdown::HandlesSealed::empty(generation),
            subscriptions_stopped,
        }
    }

    pub(crate) fn finish(
        self,
    ) -> crate::XllResult<(
        crate::shutdown::HandlesQuiescent,
        crate::shutdown::SubscriptionsStopped,
    )> {
        let handles_quiescent = self.handles.finish()?;
        Ok((handles_quiescent, self.subscriptions_stopped))
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
