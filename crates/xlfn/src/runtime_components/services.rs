//! Runtime-owned generation service slots.

use crate::generation::RuntimeGeneration;

/// Resources registered during `Addin::open` and moved into generation
/// services at commit. The value is affine: no subsystem can retain a second
/// owner when the opening transaction rolls back or publishes the generation.
pub(crate) struct GenerationServiceInputs {
    #[cfg(feature = "rtd")]
    rtd_sources: crate::subscription::SourceArena,
}

impl GenerationServiceInputs {
    #[cfg(not(feature = "rtd"))]
    pub(crate) const fn empty() -> Self {
        Self {}
    }

    #[cfg(feature = "rtd")]
    pub(crate) const fn with_rtd_sources(sources: crate::subscription::SourceArena) -> Self {
        Self {
            rtd_sources: sources,
        }
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn empty_for_generation(generation: RuntimeGeneration) -> Self {
        #[cfg(feature = "rtd")]
        {
            Self::with_rtd_sources(crate::subscription::SourceArena::empty(generation))
        }
        #[cfg(not(feature = "rtd"))]
        {
            let _ = generation;
            Self::empty()
        }
    }
}

/// Service slots whose liveness is coupled to one open generation.
/// Generation-specific policy is consumed from [`crate::generation::OpeningGeneration`]
/// while these slots carry the service state owned by the open bundle.
pub(crate) struct GenerationServices {
    #[cfg(feature = "handles")]
    formula_handles: crate::handle::FormulaHandleServiceSlot,
    #[cfg(feature = "rtd")]
    rtd: RtdGenerationServices,
}

#[cfg(feature = "rtd")]
struct RtdGenerationServices {
    subscriptions: crate::excel_rtd::SubscriptionServiceSlot,
    subscription_host: crate::excel_rtd::RtdSubscriptionHost,
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
    pub(crate) fn commit(mut self) -> Box<GenerationServices> {
        self.committed = true;
        Box::new(
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
    pub(crate) fn arm_generation(
        generation: RuntimeGeneration,
        _config: crate::addin::RuntimeConfig,
        subscription_host: Option<crate::excel_rtd::RtdSubscriptionHost>,
        inputs: GenerationServiceInputs,
    ) -> crate::XllResult<ArmedServices> {
        #[cfg(not(feature = "rtd"))]
        let _ = (generation, subscription_host, inputs);
        let services = Self {
            #[cfg(feature = "handles")]
            formula_handles: crate::handle::FormulaHandleServiceSlot::new(),
            #[cfg(feature = "rtd")]
            rtd: RtdGenerationServices {
                subscriptions: crate::excel_rtd::SubscriptionServiceSlot::new(),
                subscription_host: subscription_host
                    .expect("RTD generation services require an RTD host capability"),
            },
        };
        #[cfg(feature = "handles")]
        services.formula_handles.arm(_config.handle_config())?;
        #[cfg(feature = "rtd")]
        if let Err(error) = services
            .rtd
            .subscriptions
            .arm(generation, _config.rtd_limits(), inputs.rtd_sources)
        {
            services.disarm_or_abort();
            return Err(error);
        }
        #[cfg(feature = "handles")]
        if let Err(error) = services.formula_handles.initialize() {
            services.disarm_or_abort();
            return Err(error);
        }
        Ok(ArmedServices {
            services: Some(services),
            committed: false,
        })
    }

    #[cfg(feature = "handles")]
    pub(crate) fn handle_call_access(&self) -> crate::handle::FormulaHandleServiceResolver<'_> {
        crate::handle::FormulaHandleServiceResolver::new(&self.formula_handles)
    }

    #[cfg(feature = "rtd")]
    pub(crate) const fn rtd_call_access(&self) -> crate::rtd::RtdGenerationAccess<'_> {
        crate::rtd::RtdGenerationAccess::new(&self.rtd.subscriptions, self.rtd.subscription_host)
    }

    #[cfg(all(feature = "handles", any(test, feature = "bench-internals")))]
    pub(crate) fn formula_handle_service(
        &self,
    ) -> crate::XllResult<crate::handle::FormulaHandleServiceRead<'_>> {
        self.formula_handles.read()
    }

    /// Stop the RTD producer associated with this generation without making
    /// the formula-handle service depend on the RTD adapter.
    pub(crate) fn shutdown_handle_topics(&self) -> crate::XllResult<()> {
        #[cfg(feature = "handles")]
        {
            let Some(handles) = self.formula_handles.read_if_ready() else {
                return Ok(());
            };
            crate::excel_rtd::shutdown_handle_topics(&*handles)
        }
        #[cfg(not(feature = "handles"))]
        Ok(())
    }

    #[cfg(feature = "rtd")]
    pub(crate) fn close_subscriptions(
        &self,
        generation: Option<RuntimeGeneration>,
    ) -> crate::XllResult<crate::shutdown::SubscriptionsStopped> {
        self.rtd.subscriptions.seal(generation)
    }

    #[cfg(any(test, feature = "refinement"))]
    #[cfg(feature = "handles")]
    pub(crate) fn set_handle_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.formula_handles.set_trace_sink(trace);
    }

    #[cfg(any(test, feature = "refinement"))]
    #[cfg(feature = "rtd")]
    pub(crate) fn set_subscription_trace_sink(
        &self,
        trace: crate::shutdown_trace::ShutdownTraceHandle,
    ) {
        self.rtd.subscriptions.set_trace_sink(trace);
    }

    pub(crate) fn seal(
        &self,
        generation: Option<RuntimeGeneration>,
        subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    ) -> crate::XllResult<SealedGenerationServices> {
        #[cfg(feature = "handles")]
        let handles = self.formula_handles.seal(generation)?;
        #[cfg(not(feature = "handles"))]
        let handles = crate::shutdown::HandlesSealed::empty(generation);
        Ok(SealedGenerationServices {
            handles,
            subscriptions_stopped,
        })
    }

    pub(crate) fn is_none(&self) -> bool {
        #[cfg(feature = "handles")]
        let handles_none = self.formula_handles.is_none();
        #[cfg(not(feature = "handles"))]
        let handles_none = true;
        #[cfg(feature = "rtd")]
        let subscriptions_none = self.rtd.subscriptions.is_none();
        #[cfg(not(feature = "rtd"))]
        let subscriptions_none = true;
        handles_none && subscriptions_none
    }

    pub(crate) fn disarm_or_abort(&self) {
        if let Err(error) = self.disarm_generation() {
            tracing::error!(%error, "generation service rollback violated its state invariant");
            std::process::abort();
        }
    }

    pub(crate) fn disarm_generation(&self) -> crate::XllResult<()> {
        #[cfg(feature = "handles")]
        let handle_result = self.formula_handles.disarm();
        #[cfg(not(feature = "handles"))]
        let handle_result: crate::XllResult<()> = Ok(());
        #[cfg(feature = "rtd")]
        let subscription_result = self.rtd.subscriptions.disarm();
        #[cfg(not(feature = "rtd"))]
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
