//! Runtime-owned generation service slots.

use crate::RtdSubscription;

use crate::generation::RuntimeGeneration;

// No handles feature means no handle owner or external pins exist. These
// vacuous certificates are separate from the enabled implementation's issuer.
#[cfg(not(feature = "handles"))]
pub(crate) struct HandlesSealed {
    generation: Option<RuntimeGeneration>,
}
#[cfg(not(feature = "handles"))]
#[derive(Debug)]
pub(crate) struct HandlesQuiescent {
    generation: Option<RuntimeGeneration>,
}
#[cfg(not(feature = "handles"))]
impl HandlesSealed {
    fn empty(generation: Option<RuntimeGeneration>) -> Self {
        Self { generation }
    }
    fn absent_services<A: crate::Addin>(
        control: &crate::lifecycle::LifecycleAccess<'_, A>,
    ) -> crate::XllResult<Self> {
        Ok(Self::empty(control.protocol_generation()))
    }
    pub(crate) fn finish(self) -> crate::XllResult<HandlesQuiescent> {
        Ok(HandlesQuiescent {
            generation: self.generation,
        })
    }
}
#[cfg(not(feature = "handles"))]
impl HandlesQuiescent {
    pub(crate) const fn generation(&self) -> Option<RuntimeGeneration> {
        self.generation
    }
}

/// Resources registered during `Addin::open` and moved into generation
/// services at commit. The value is affine: no subsystem can retain a second
/// owner when the opening transaction rolls back or publishes the generation.
pub(crate) struct GenerationServiceInputs<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    marker: std::marker::PhantomData<fn() -> (E, C)>,
    #[cfg(feature = "rtd")]
    rtd_sources: crate::subscription::SourceArena<E, C>,
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> GenerationServiceInputs<E, C> {
    #[cfg(not(feature = "rtd"))]
    pub(crate) const fn empty() -> Self {
        Self {
            marker: std::marker::PhantomData,
        }
    }

    #[cfg(feature = "rtd")]
    pub(crate) const fn with_rtd_sources(sources: crate::subscription::SourceArena<E, C>) -> Self {
        Self {
            marker: std::marker::PhantomData,
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
pub(crate) struct GenerationServices<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    generation: RuntimeGeneration,
    marker: std::marker::PhantomData<fn() -> (E, C)>,
    #[cfg(feature = "handles")]
    formula_handles: crate::handle::FormulaHandleServiceSlot,
    #[cfg(feature = "rtd")]
    rtd: RtdGenerationServices<E, C>,
}

#[cfg(feature = "rtd")]
struct RtdGenerationServices<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    subscriptions: crate::excel_rtd::SubscriptionServiceSlot<E, C>,
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

/// Reservation for a generation-owned service bundle.
///
/// Before publication this owner can destroy the exclusive service bundle.
/// Slot destruction drains its own readers and reclaims outside its locks;
/// it also handles a handle slot already initialized to Ready.
#[must_use = "an armed service reservation must be committed or rolled back"]
pub(crate) struct ArmedServices<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
> {
    services: GenerationServices<E, C>,
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> ArmedServices<E, C> {
    pub(crate) fn commit(self) -> Box<GenerationServices<E, C>> {
        Box::new(self.services)
    }
}

impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> GenerationServices<E, C> {
    pub(crate) fn arm_generation(
        generation: RuntimeGeneration,
        _config: crate::addin::RuntimeConfig,
        #[cfg(feature = "rtd")] subscription_host: crate::excel_rtd::RtdSubscriptionHost,
        inputs: GenerationServiceInputs<E, C>,
    ) -> crate::XllResult<ArmedServices<E, C>> {
        #[cfg(not(feature = "rtd"))]
        let _ = (generation, inputs);
        let services = Self {
            generation,
            marker: std::marker::PhantomData,
            #[cfg(feature = "handles")]
            formula_handles: crate::handle::FormulaHandleServiceSlot::new(),
            #[cfg(feature = "rtd")]
            rtd: RtdGenerationServices {
                subscriptions: crate::excel_rtd::SubscriptionServiceSlot::new(),
                subscription_host,
            },
        };
        #[cfg(feature = "handles")]
        services.formula_handles.arm(_config.handle_config())?;
        #[cfg(feature = "rtd")]
        services
            .rtd
            .subscriptions
            .arm(generation, _config.rtd_limits(), inputs.rtd_sources)?;
        #[cfg(feature = "handles")]
        services.formula_handles.initialize()?;
        Ok(ArmedServices { services })
    }

    #[cfg(feature = "handles")]
    pub(crate) fn handle_call_access(&self) -> crate::handle::FormulaHandleServiceResolver<'_> {
        crate::handle::FormulaHandleServiceResolver::new(&self.formula_handles)
    }

    #[cfg(feature = "rtd")]
    pub(crate) const fn rtd_call_access(&self) -> crate::rtd::RtdGenerationAccess<'_, E, C> {
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
            crate::excel_rtd::shutdown_handle_topics(&handles)
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
        subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    ) -> crate::XllResult<SealedGenerationServices> {
        if subscriptions_stopped.generation() != Some(self.generation) {
            return Err(crate::XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_CERTIFICATE,
            });
        }
        #[cfg(feature = "handles")]
        let handles = self.formula_handles.seal(self.generation)?;
        #[cfg(not(feature = "handles"))]
        let handles = HandlesSealed::empty(Some(self.generation));
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
}

impl SealedGenerationServices {
    /// Derive a no-services result from canonical state while its lock is
    /// retained. Callers cannot select an arbitrary empty generation.
    pub(crate) fn seal_lifecycle<A: crate::Addin>(
        lifecycle: &crate::lifecycle::LifecycleCoordinator<A>,
        subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    ) -> crate::XllResult<Self> {
        let control = lifecycle.access();
        if let Some(services) = control.generation_services() {
            return services.seal(subscriptions_stopped);
        }
        Ok(Self {
            handles: crate::shutdown::HandlesSealed::absent_services(&control)?,
            subscriptions_stopped,
        })
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

#[cfg(all(test, feature = "handles"))]
mod tests {
    use super::*;

    fn armed() -> ArmedServices {
        let generation = RuntimeGeneration::new(1).unwrap();
        GenerationServices::arm_generation(
            generation,
            crate::addin::RuntimeConfig::default(),
            #[cfg(feature = "rtd")]
            crate::excel_rtd::RtdSubscriptionHost::production(crate::module_runtime::ingress()),
            GenerationServiceInputs::empty_for_generation(generation),
        )
        .unwrap()
    }

    #[test]
    fn miri_unpublished_ready_service_bundle_can_be_abandoned() {
        let reservation = armed();
        assert!(
            reservation
                .services
                .formula_handles
                .read_if_ready()
                .is_some()
        );
        // A Ready service is not a Cold disarm transaction. Exclusive slot
        // destruction supplies drain/reclaim authority instead.
        drop(reservation);
    }

    #[test]
    fn miri_rejected_unpublished_service_bundle_destroys_its_ready_slot() {
        let services = armed().commit();
        assert!(services.formula_handles.read_if_ready().is_some());
        drop(services);
    }

    #[test]
    #[allow(
        unsafe_code,
        reason = "trusted test restores only its own forgotten unique allocation after checking retention"
    )]
    fn miri_forgotten_unpublished_services_do_not_reclaim_early() {
        // `into_raw` is the recoverable form of forgetting the unique Box. It
        // keeps provenance valid across ownership transfer, unlike a pointer
        // borrowed from a Box before moving that Box into mem::forget.
        let retained = Box::into_raw(Box::new(armed()));
        // SAFETY: transferring the unique Box left this allocation live.
        let service = unsafe { &*retained };
        assert!(service.services.formula_handles.read_if_ready().is_some());
        // SAFETY: the test restores the single forgotten Box after its
        // observation, so Miri can also verify final destruction without leaks.
        unsafe { drop(Box::from_raw(retained)) };
    }
}
