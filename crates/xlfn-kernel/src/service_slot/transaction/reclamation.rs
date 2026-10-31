//! Trusted conversion from the exact slot/lane drain to allocation recovery.
//! Owner fields and drained constructors are private here; orchestration can
//! retain, withdraw or quarantine an owner, but cannot recover it on its own.
use super::*;
use std::mem::ManuallyDrop;

// Published/withdrawn/quarantined owners deliberately do not destroy their
// allocation on field Drop. Only a drained or exclusively destroyed slot may
// recover it. Forgetting any intermediate capability therefore retains data.
pub(in crate::service_slot) struct PublishedServiceOwner<R>(ManuallyDrop<PublishedOwner<R>>);
pub(super) struct WithdrawnServiceOwner<R>(ManuallyDrop<PublishedOwner<R>>);
pub(in crate::service_slot) struct QuarantinedServiceOwner<R>(ManuallyDrop<PublishedOwner<R>>);

impl<R> PublishedServiceOwner<R> {
    pub(super) fn publish(owner: PublishedOwner<R>) -> Self {
        Self(ManuallyDrop::new(owner))
    }
    pub(in crate::service_slot) fn as_ptr(&self) -> *mut R {
        self.0.as_ptr()
    }
    fn withdraw(self) -> WithdrawnServiceOwner<R> {
        WithdrawnServiceOwner(self.0)
    }
}
impl<R> WithdrawnServiceOwner<R> {
    pub(super) fn as_ref(&self) -> &R {
        self.0.as_ref()
    }
    pub(super) fn quarantine(self) -> QuarantinedServiceOwner<R> {
        QuarantinedServiceOwner(self.0)
    }
}
impl<R> QuarantinedServiceOwner<R> {
    fn retain(self) {
        // Intentionally end only the wrapper's ownership bookkeeping. The
        // ManuallyDrop field never runs arbitrary quarantined payload Drop.
        let _retained = self.0;
    }

    #[cfg(test)]
    pub(in crate::service_slot) fn recover_for_test(self) -> PublishedOwner<R> {
        ManuallyDrop::into_inner(self.0)
    }
}

struct Sealing<'slot, C, R, E: Clone> {
    slot: &'slot GenerationServiceSlot<C, R, E>,
    runtime: WithdrawnServiceOwner<R>,
}

impl<C, R, E: Clone> Sealing<'_, C, R, E> {
    fn retain(self, fault: Arc<ServiceFault<E>>) {
        let mut state = self.slot.state.lock();
        if !matches!(&*state, GenerationServiceState::Sealing) {
            crate::invariant::fail_stop();
        }
        *state = GenerationServiceState::TeardownFaulted {
            fault,
            runtime: self.runtime.quarantine(),
        };
        self.slot.changed.notify_all();
    }
}

impl<C, R, E: Clone> Rollback for Sealing<'_, C, R, E> {
    fn rollback(self) {
        // Also valid BEFORE reader drain: retain, never destroy, the owner.
        self.retain(Arc::new(ServiceFault::Panicked));
    }
}

#[must_use = "wait for this service's readers before finishing shutdown"]
pub(in crate::service_slot) struct SealingTxn<'slot, C, R, E: Clone> {
    recovery: AbortGuard<Sealing<'slot, C, R, E>>,
}

#[must_use = "finish shutdown or retain the quiesced owner as a fault"]
pub(in crate::service_slot) struct QuiescedTxn<'slot, C, R, E: Clone> {
    recovery: AbortGuard<Sealing<'slot, C, R, E>>,
}

impl<'slot, C, R, E: Clone> SealingTxn<'slot, C, R, E> {
    pub(in crate::service_slot) fn wait(self) -> super::QuiescedTxn<'slot, C, R, E> {
        // [SS-4]: the owner and the gate originate from the SAME withdrawal.
        drop(self.recovery.state().slot.readers.control().seal().wait());
        QuiescedTxn {
            recovery: self.recovery,
        }
    }
}

impl<C, R, E: Clone> QuiescedTxn<'_, C, R, E> {
    pub(in crate::service_slot) fn finish<S>(
        self,
        shutdown: impl FnOnce(&R) -> Result<S, E>,
    ) -> Result<ServiceSeal<R, S>, ServiceSlotError<E>> {
        let result = shutdown(self.recovery.state().runtime.as_ref());
        match result {
            Ok(sealed) => {
                let slot = self.recovery.state().slot;
                let mut state = slot.state.lock();
                *state = GenerationServiceState::Closed;
                slot.changed.notify_all();
                drop(state);
                let Sealing { runtime, .. } = self.recovery.complete();
                // Only this consuming, quiesced state may recover the Box.
                Ok(ServiceSeal::Present {
                    runtime: ManuallyDrop::into_inner(runtime.0).into_box(),
                    sealed,
                })
            }
            Err(error) => {
                // Clone stays outside the lock AND under the recovery guard.
                let fault = Arc::new(ServiceFault::Error(error.clone()));
                self.recovery.complete().retain(fault);
                Err(ServiceSlotError::Fault(ServiceFault::Error(error)))
            }
        }
    }
}

#[must_use]
pub(in crate::service_slot) struct DrainedReplacement<'slot, R> {
    transition: Transition<'slot, R>,
}
impl<'slot, R> RetiringReplacement<'slot, R> {
    pub(in crate::service_slot) fn wait(self) -> super::DrainedReplacement<'slot, R> {
        if let Some(index) = self.transition.previous {
            drop(
                self.transition.slot.lanes[index]
                    .readers
                    .control()
                    .seal()
                    .wait(),
            );
        }
        DrainedReplacement {
            transition: self.transition,
        }
    }
}
impl<R> DrainedReplacement<'_, R> {
    pub(in crate::service_slot) fn reclaim(self) -> Option<RetiredService<R>> {
        self.transition.previous.map(|index| {
            let lane = &self.transition.slot.lanes[index];
            // Delayed admitted readers must see this pointer until drain ends.
            lane.published
                .store(std::ptr::null_mut(), Ordering::Release);
            let owner = lane
                .owner
                .lock()
                .take()
                .unwrap_or_else(|| crate::invariant::fail_stop());
            RetiredService::new(ManuallyDrop::into_inner(owner.0).into_box())
        })
    }
}

impl<C, R, E> Drop for GenerationServiceSlot<C, R, E> {
    fn drop(&mut self) {
        let state = {
            let _drained = self.readers.control().seal().wait();
            self.published
                .store(std::ptr::null_mut(), Ordering::Release);
            std::mem::replace(self.state.get_mut(), GenerationServiceState::Closed)
        };
        // Exclusive slot destruction supplies the lifetime/issuer authority;
        // user payload and config/error destructors run outside all locks.
        match state {
            GenerationServiceState::Ready { runtime } => {
                drop(ManuallyDrop::into_inner(runtime.0).into_box())
            }
            GenerationServiceState::TeardownFaulted { runtime, .. } => runtime.retain(),
            retired => drop(retired),
        }
    }
}
impl<R> Drop for ReplaceableServiceSlot<R> {
    fn drop(&mut self) {
        self.active.store(NO_ACTIVE_LANE, Ordering::Release);
        for lane in &mut self.lanes {
            let owner = {
                let _drained = lane.readers.control().seal().wait();
                lane.published
                    .store(std::ptr::null_mut(), Ordering::Release);
                lane.owner.get_mut().take()
            };
            if let Some(owner) = owner {
                drop(ManuallyDrop::into_inner(owner.0).into_box());
            }
        }
    }
}

impl<C, R, E: Clone> GenerationServiceSlot<C, R, E> {
    pub(super) fn begin_seal(
        &self,
    ) -> Result<Option<super::SealingTxn<'_, C, R, E>>, ServiceSlotError<E>> {
        let mut state = self.state.lock();
        while matches!(
            *state,
            GenerationServiceState::Initializing | GenerationServiceState::Sealing
        ) {
            self.changed.wait(&mut state);
        }

        if let GenerationServiceState::TeardownFaulted { fault, .. } = &*state {
            let fault = Arc::clone(fault);
            drop(state);
            return Err(ServiceSlotError::Fault(fault.as_ref().clone()));
        }

        Ok(
            match std::mem::replace(&mut *state, GenerationServiceState::Sealing) {
                GenerationServiceState::Ready { runtime } => {
                    // Formal theorem [SS-3]: Withdraw publication and seal readers gate
                    // before waiting for drain, preventing any late reader admission.
                    drop(self.readers.control().seal());
                    self.published
                        .store(std::ptr::null_mut(), Ordering::Release);
                    Some(SealingTxn {
                        recovery: AbortGuard::new(Sealing {
                            slot: self,
                            runtime: runtime.withdraw(),
                        }),
                    })
                }
                retired @ (GenerationServiceState::Cold { .. }
                | GenerationServiceState::InitFaulted { .. }
                | GenerationServiceState::Closed) => {
                    *state = GenerationServiceState::Closed;
                    self.changed.notify_all();
                    drop(state);
                    drop(retired);
                    return Ok(None);
                }
                GenerationServiceState::Initializing
                | GenerationServiceState::Sealing
                | GenerationServiceState::TeardownFaulted { .. } => {
                    unreachable!()
                }
            },
        )
    }
}
