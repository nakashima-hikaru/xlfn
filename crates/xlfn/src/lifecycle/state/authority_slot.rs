//! Installation after one locked acquisition of an empty authority slot.
use super::*;

enum VacantClose<A: crate::Addin> {
    Unclaimed(ClosePayload<A>),
    Claimed {
        payload: ClosePayload<A>,
        claim: OutstandingModuleClaim,
    },
}

impl<A: crate::Addin> VacantClose<A> {
    #[allow(
        clippy::result_large_err,
        reason = "acquisition refusal returns the complete original receiver without allocating"
    )]
    fn acquire(
        resources: CloseResources<A>,
        epoch: ModuleEpochId,
    ) -> Result<Self, CloseResources<A>> {
        match resources {
            CloseResources::AwaitingOpenAbort { payload, .. } => {
                Ok(Self::Unclaimed(payload.into_close_payload()))
            }
            CloseResources::Unowned { payload } => Ok(Self::Unclaimed(payload)),
            CloseResources::Claimed(ClaimedClose {
                payload,
                claim: RemovalClaimState::Outstanding(claim),
            }) if claim.module_epoch == epoch => Ok(Self::Claimed { payload, claim }),
            other => Err(other),
        }
    }

    fn install(self, authority: ModuleAuthority) -> CloseResources<A> {
        match self {
            Self::Unclaimed(payload) => CloseResources::with_authority(payload, authority),
            Self::Claimed { payload, claim } => CloseResources::Claimed(ClaimedClose {
                payload,
                claim: RemovalClaimState::Returned(ReturnedModuleClaim {
                    attempt: claim.attempt,
                    authority,
                }),
            }),
        }
    }
}

enum Destination {
    Closing,
    Rollback,
    Quarantine,
}

impl Destination {
    fn restore<A: crate::Addin>(self, resources: CloseResources<A>) -> LifecycleState<A> {
        match self {
            Self::Closing => LifecycleState::Closing(ClosingState::Ready { resources }),
            Self::Rollback => LifecycleState::OpenRollbackPending(resources),
            Self::Quarantine => LifecycleState::Quarantined(resources),
        }
    }
}

/// Both the empty receiver and the particular authority have already been
/// joined under the canonical lock. Existing authority and incompatible
/// lifecycle phases cannot be represented by this installation receiver.
pub(super) struct PreparedAuthorityInstall<A: crate::Addin> {
    parts: std::mem::ManuallyDrop<InstallParts<A>>,
}

struct InstallParts<A: crate::Addin> {
    receiver: VacantClose<A>,
    destination: Destination,
    authority: ModuleAuthority,
}

impl<A: crate::Addin> PreparedAuthorityInstall<A> {
    #[allow(
        clippy::result_large_err,
        reason = "acquisition refusal retains both the original canonical state and incoming authority"
    )]
    pub(super) fn acquire(
        state: LifecycleState<A>,
        authority: ModuleAuthority,
    ) -> Result<Self, (LifecycleState<A>, ModuleAuthority)> {
        let (resources, destination) = match state {
            LifecycleState::Closed(ClosedState::Idle) => (
                CloseResources::Unowned {
                    payload: ClosePayload::Empty,
                },
                Destination::Closing,
            ),
            LifecycleState::Opening {
                payload: OpeningPayload::Empty,
                ..
            } => (
                CloseResources::Unowned {
                    payload: ClosePayload::Empty,
                },
                Destination::Rollback,
            ),
            LifecycleState::Opening {
                payload: OpeningPayload::Staged(opening),
                ..
            } => (
                CloseResources::Unowned {
                    payload: ClosePayload::Staged(opening),
                },
                Destination::Rollback,
            ),
            LifecycleState::Closing(ClosingState::OpeningActive {
                quarantine_requested,
                resources: ActiveOpeningClose::AwaitingAbort { payload },
                ..
            }) => (
                CloseResources::Unowned {
                    payload: payload.into_close_payload(),
                },
                if quarantine_requested {
                    Destination::Quarantine
                } else {
                    Destination::Closing
                },
            ),
            LifecycleState::Closing(ClosingState::Ready { resources }) => {
                (resources, Destination::Closing)
            }
            LifecycleState::OpenRollbackPending(resources) => (resources, Destination::Rollback),
            LifecycleState::Quarantined(resources) => (resources, Destination::Quarantine),
            other => return Err((other, authority)),
        };
        match VacantClose::acquire(resources, authority.id()) {
            Ok(receiver) => Ok(Self {
                parts: std::mem::ManuallyDrop::new(InstallParts {
                    receiver,
                    destination,
                    authority,
                }),
            }),
            Err(resources) => Err((destination.restore(resources), authority)),
        }
    }

    pub(super) fn install(self) -> LifecycleState<A> {
        let InstallParts {
            receiver,
            destination,
            authority,
        } = std::mem::ManuallyDrop::into_inner(self.parts);
        destination.restore(receiver.install(authority))
    }
}

#[cfg(test)]
#[allow(
    clippy::drop_non_drop,
    clippy::forget_non_drop,
    reason = "tests exercise ordinary field destruction and forget of the ManuallyDrop-owned intermediate"
)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct DropProbe(Arc<AtomicUsize>);
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    struct PayloadAddin;
    impl crate::Addin for PayloadAddin {
        #[cfg(feature = "rtd")]
        type RtdSubscription = ();
        #[cfg(feature = "rtd")]
        type RtdSources = crate::DynamicRtdSources<()>;
        type SharedState = DropProbe;
        type LifecycleState = ();
        type Error = crate::XllError;
        type Layers = ();
        #[cfg(feature = "async")]
        type AsyncExecutor = <() as crate::Addin>::AsyncExecutor;
        fn open(_: &crate::OpenContext<Self>) -> crate::OpenResult<Self> {
            Err(crate::XllError::Closing)
        }
    }

    fn published_receiver(drops: Arc<AtomicUsize>) -> LifecycleState<PayloadAddin> {
        let generation = RuntimeGeneration::new(1).unwrap();
        let root = PublishedOwner::new(ExecutionGeneration::<PayloadAddin> {
            id: generation,
            shared_state: DropProbe(drops),
            layers: (),
        });
        let services = PublishedOwner::from_box(
            GenerationServices::arm_generation(
                generation,
                crate::addin_impl::RuntimeConfig::default(),
                #[cfg(feature = "rtd")]
                crate::excel_rtd::RtdSubscriptionHost::production(crate::module_runtime::ingress()),
                crate::runtime_components::GenerationServiceInputs::empty_for_generation(
                    generation,
                ),
            )
            .unwrap()
            .commit(),
        );
        let publication = PublishedOwner::new(PublishedGeneration::new(&root, &services));
        LifecycleState::Quarantined(CloseResources::Unowned {
            payload: ClosePayload::Published(ClosingGeneration {
                generation: root,
                services,
                publication,
            }),
        })
    }

    fn authority() -> ModuleAuthority {
        ModuleAuthority::Closing(crate::module_runtime::isolated_closing_for_test())
    }

    struct StagedPayloadAddin;
    impl crate::Addin for StagedPayloadAddin {
        #[cfg(feature = "rtd")]
        type RtdSubscription = ();
        #[cfg(feature = "rtd")]
        type RtdSources = crate::DynamicRtdSources<()>;
        type SharedState = PublishedOwner<DropProbe>;
        type LifecycleState = ();
        type Error = crate::XllError;
        type Layers = ();
        #[cfg(feature = "async")]
        type AsyncExecutor = <() as crate::Addin>::AsyncExecutor;
        fn open(_: &crate::OpenContext<Self>) -> crate::OpenResult<Self> {
            Err(crate::XllError::Closing)
        }
    }

    #[test]
    fn miri_abandoned_prepared_install_retains_then_recovers_staged_payload() {
        let _module_lease = crate::ingress::acquire_test_module_lease();
        for unwind in [false, true] {
            let drops = Arc::new(AtomicUsize::new(0));
            let payload = PublishedOwner::new(DropProbe(Arc::clone(&drops)));
            let retained_pointer = payload.as_ptr();
            let state =
                LifecycleState::<StagedPayloadAddin>::Quarantined(CloseResources::Unowned {
                    payload: ClosePayload::Staged(OpeningGeneration {
                        shared_state: payload,
                        layers: (),
                        init_config: crate::addin_impl::RuntimeConfig::default(),
                    }),
                });
            // This fixture does not initialize generation services or publish
            // readers. Complete the unrelated module barrier first, so losing
            // its drained authority cannot leave a live ingress epoch behind.
            let drained = crate::module_runtime::begin_open()
                .rollback(|| {})
                .seal_and_drain()
                .unwrap_or_else(|_| panic!("fixture module drain failed"));
            let prepared =
                PreparedAuthorityInstall::acquire(state, ModuleAuthority::Drained(drained))
                    .unwrap_or_else(|_| panic!("vacant staged receiver refused"));
            if unwind {
                assert!(
                    crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                        let _prepared = prepared;
                        panic!("after protected receiver acquisition");
                    }))
                    .is_err()
                );
            } else {
                drop(prepared);
            }
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            // SAFETY: the ManuallyDrop owner disappeared without freeing this
            // allocation. The fixture never published it, admitted a reader,
            // or retained another owner; this is its sole test-only recovery.
            drop(unsafe { Box::from_raw(retained_pointer) });
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "abandoned authority installation must retain published payloads"
    )]
    fn prepared_install_abandonment_never_destroys_a_published_payload() {
        for mode in 0..3 {
            let drops = Arc::new(AtomicUsize::new(0));
            let prepared = PreparedAuthorityInstall::acquire(
                published_receiver(Arc::clone(&drops)),
                authority(),
            )
            .unwrap_or_else(|_| panic!("vacant published receiver refused"));
            match mode {
                0 => drop(prepared),
                1 => assert!(
                    crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                        let _prepared = prepared;
                        panic!("after vacant receiver acquisition");
                    }))
                    .is_err()
                ),
                _ => std::mem::forget(prepared),
            }
            assert_eq!(drops.load(Ordering::SeqCst), 0);
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let prepared =
            PreparedAuthorityInstall::acquire(published_receiver(Arc::clone(&drops)), authority())
                .unwrap_or_else(|_| panic!("vacant published receiver refused"));
        let state = prepared.install();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        // This fixture has no admitted readers; dropping the fully installed
        // state proves installation transferred, rather than duplicated, root ownership.
        drop(state);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn occupied_receiver_rejects_without_displacing_either_authority() {
        let original = authority();
        let original_epoch = original.id();
        let incoming = authority();
        let incoming_epoch = incoming.id();
        let state = LifecycleState::<()>::Quarantined(CloseResources::with_authority(
            ClosePayload::Empty,
            original,
        ));
        let Err((state, incoming)) = PreparedAuthorityInstall::acquire(state, incoming) else {
            panic!("occupied receiver accepted another authority");
        };
        assert_eq!(state.module_epoch_id(), Some(original_epoch));
        assert_eq!(incoming.id(), incoming_epoch);
        assert_eq!(state.phase(), LifecyclePhase::Quarantined);
        let vault = crate::runtime_components::QuarantineVault::<()>::new();
        vault.retain_module_authority(incoming);
        assert_eq!(vault.snapshot().len(), 1);
    }

    #[test]
    fn outstanding_receiver_requires_the_same_epoch_and_transfers_once() {
        let original = authority();
        let epoch = original.id();
        let incoming = authority();
        let incoming_epoch = incoming.id();
        let attempt = RemovalAttemptId::new(7).unwrap();
        let state = LifecycleState::<()>::Quarantined(CloseResources::Claimed(ClaimedClose {
            payload: ClosePayload::Empty,
            claim: RemovalClaimState::Outstanding(OutstandingModuleClaim {
                attempt,
                module_epoch: epoch,
            }),
        }));
        let Err((state, incoming)) = PreparedAuthorityInstall::acquire(state, incoming) else {
            panic!("different module epoch accepted");
        };
        assert_eq!(state.module_epoch_id(), Some(epoch));
        assert_eq!(state.removal_attempt(), Some(attempt));
        assert_eq!(incoming.id(), incoming_epoch);
        let installation = PreparedAuthorityInstall::acquire(state, original)
            .unwrap_or_else(|_| panic!("original epoch refused"));
        let state = installation.install();
        assert!(matches!(
            state,
            LifecycleState::Quarantined(CloseResources::Claimed(ClaimedClose {
                claim: RemovalClaimState::Returned(_),
                ..
            }))
        ));
        let Err((state, incoming)) = PreparedAuthorityInstall::acquire(state, incoming) else {
            panic!("returned receiver accepted a second authority");
        };
        assert_eq!(state.module_epoch_id(), Some(epoch));
        let vault = crate::runtime_components::QuarantineVault::<()>::new();
        vault.retain_module_authority(incoming);
    }

    #[test]
    fn finalizing_and_foreign_access_rejection_preserve_the_target() {
        let issuer = LifecycleCoordinator::<()>::new();
        let other = LifecycleCoordinator::<()>::new();
        let attempt = RemovalAttemptId::new(11).unwrap();
        {
            let mut access = issuer.access();
            access.transition(|core| {
                core.state = LifecycleState::Closed(ClosedState::Finalizing {
                    attempt,
                    quarantine_requested: true,
                });
                ((), TransitionEffect::Keep)
            });
        }
        let incoming = authority();
        let epoch = incoming.id();
        let mut foreign = other.access();
        let incoming = issuer
            .install_returned_authority(&mut foreign, incoming)
            .expect_err("foreign access must not install authority");
        assert_eq!(incoming.id(), epoch);
        assert_eq!(foreign.phase(), LifecyclePhase::Closed);
        drop(foreign);
        let mut access = issuer.access();
        let incoming = issuer
            .install_returned_authority(&mut access, incoming)
            .expect_err("finalizing receiver must not install authority");
        assert_eq!(incoming.id(), epoch);
        assert_eq!(access.removal_attempt(), Some(attempt));
        assert!(matches!(
            access.canonical_state(),
            LifecycleState::Closed(ClosedState::Finalizing {
                quarantine_requested: true,
                ..
            })
        ));
        let vault = crate::runtime_components::QuarantineVault::<()>::new();
        vault.retain_module_authority(incoming);
    }
}
