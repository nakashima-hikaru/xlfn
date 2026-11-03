//! Exclusive occupancy changes. Access witnesses cannot construct or retarget
//! these claims; one live claim pins the binding's occupancy protocol.
use super::*;

pub struct BindingEmpty;
pub struct BindingOccupied;
pub struct BindingReleased;

pub enum BindingState<'slot, T: 'static> {
    Empty(BindingTxn<'slot, T, BindingEmpty>),
    Occupied(BindingTxn<'slot, T, BindingOccupied>),
}

struct BindingClaim<'slot, T: 'static> {
    access: ThreadAffineAccess<'slot, T>,
}

impl<T: 'static> BindingClaim<'_, T> {
    fn install_claimed(&self, value: T) -> Result<(), ThreadAffineInstallError<T>> {
        let slot_id = self.access.slot.id();
        let mut value = Some(value);
        let result = with_values(|values| {
            if values.iter().any(|entry| entry.slot_id == slot_id) {
                return Err(ThreadAffineError::Occupied);
            }
            values.push(ThreadAffineEntry {
                slot_id,
                value: Some(Box::new(
                    value
                        .take()
                        .expect("thread-affine install value is present"),
                )),
            });
            Ok(())
        });

        match result {
            Ok(()) => Ok(()),
            Err(reason) => Err(ThreadAffineInstallError {
                value: value.expect("failed thread-affine install retains its value"),
                reason,
            }),
        }
    }

    fn take_claimed(&self) -> Result<T, ThreadAffineError> {
        let slot_id = self.access.slot.id();
        with_values(|values| {
            let index = values
                .iter()
                .position(|entry| entry.slot_id == slot_id)
                .ok_or(ThreadAffineError::MissingValue)?;
            let value = values[index]
                .value
                .as_ref()
                .ok_or(ThreadAffineError::MissingValue)?;
            if !value.is::<T>() {
                return Err(ThreadAffineError::TypeMismatch);
            }
            let mut entry = values.swap_remove(index);
            let value = entry.value.take().ok_or(ThreadAffineError::MissingValue)?;
            #[allow(
                clippy::mem_forget,
                reason = "entry.value was extracted; dropping the empty entry wrapper must not invoke TLS fallback cleanup"
            )]
            std::mem::forget(entry);
            match value.downcast::<T>() {
                Ok(value) => Ok(*value),
                Err(value) => {
                    #[allow(
                        clippy::mem_forget,
                        reason = "type mismatch on dynamic downcast; avoid invoking add-in Drop if type is unexpected"
                    )]
                    std::mem::forget(value);
                    Err(ThreadAffineError::TypeMismatch)
                }
            }
        })
    }

    fn release_empty_claimed(&self) -> Result<(), ThreadAffineError> {
        let slot_id = self.access.slot.id();
        let occupied =
            with_values(|values| Ok(values.iter().any(|entry| entry.slot_id == slot_id)))?;
        if occupied {
            return Err(ThreadAffineError::Occupied);
        }
        // A live non-Send claim prevents rebinding or another occupancy
        // transition until its Drop clears modifying. The owner and binding
        // therefore still match the acquisition; retain the mutex for the
        // synchronized update and keep modifying set through claim destruction.
        self.access.slot.affinity.lock().owner = None;
        Ok(())
    }
}

impl<T: 'static> Drop for BindingClaim<'_, T> {
    fn drop(&mut self) {
        let mut affinity = self.access.slot.affinity.lock();
        if affinity.binding == self.access.binding {
            affinity.modifying = false;
        }
    }
}

/// Exclusive, non-Send occupancy authority. Forgetting it conservatively
/// retains the binding claim and any TLS payload; it never destroys a value.
#[must_use = "complete or retain the binding transition"]
pub struct BindingTxn<'slot, T: 'static, State> {
    claim: BindingClaim<'slot, T>,
    _state: PhantomData<State>,
}

impl<T: 'static, State> std::fmt::Debug for BindingTxn<'_, T, State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BindingTxn").finish_non_exhaustive()
    }
}

pub struct BindingTransitionError<'slot, T: 'static, State> {
    pub transaction: BindingTxn<'slot, T, State>,
    pub reason: ThreadAffineError,
}

impl<T: 'static, State> std::fmt::Debug for BindingTransitionError<'_, T, State> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BindingTransitionError")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct BindingInstallError<'slot, T: 'static> {
    pub transaction: BindingTxn<'slot, T, BindingEmpty>,
    pub value: T,
    pub reason: ThreadAffineError,
}

impl<'slot, T: 'static, State> BindingTxn<'slot, T, State> {
    /// Observes the issuer without exposing or transferring mutation authority.
    pub fn belongs_to(&self, slot: &ThreadAffineSlot<T>) -> bool {
        std::ptr::eq(self.claim.access.slot, slot)
    }

    fn next<Next>(self) -> BindingTxn<'slot, T, Next> {
        BindingTxn {
            claim: self.claim,
            _state: PhantomData,
        }
    }
}

impl<'slot, T: 'static> BindingTxn<'slot, T, BindingEmpty> {
    pub fn install(
        self,
        value: T,
    ) -> Result<BindingTxn<'slot, T, BindingOccupied>, BindingInstallError<'slot, T>> {
        match self.claim.install_claimed(value) {
            Ok(()) => Ok(self.next()),
            Err(error) => Err(BindingInstallError {
                transaction: self,
                value: error.value,
                reason: error.reason,
            }),
        }
    }

    pub fn release(
        self,
    ) -> Result<BindingReleased, BindingTransitionError<'slot, T, BindingEmpty>> {
        match self.claim.release_empty_claimed() {
            Ok(()) => Ok(BindingReleased),
            Err(reason) => Err(BindingTransitionError {
                transaction: self,
                reason,
            }),
        }
    }
}

impl<'slot, T: 'static> BindingTxn<'slot, T, BindingOccupied> {
    pub fn take(
        self,
    ) -> Result<
        (T, BindingTxn<'slot, T, BindingEmpty>),
        BindingTransitionError<'slot, T, BindingOccupied>,
    > {
        match self.claim.take_claimed() {
            Ok(value) => Ok((value, self.next())),
            Err(reason) => Err(BindingTransitionError {
                transaction: self,
                reason,
            }),
        }
    }
}

impl<T: 'static> ThreadAffineSlot<T> {
    /// Acquires the single occupancy-change claim for this exact binding.
    /// Callbacks can retain access witnesses, but cannot invalidate this claim's
    /// phase through a sibling witness. No TLS borrow or mutex is held by it.
    pub fn binding<'slot>(
        &'slot self,
        access: &ThreadAffineAccess<'_, T>,
    ) -> Result<BindingState<'slot, T>, ThreadAffineError> {
        // Pointer provenance must be checked even for an otherwise valid
        // access issued by a different slot/runtime.
        if !std::ptr::eq(self, access.slot) {
            return Err(ThreadAffineError::StaleAccess);
        }
        let current = thread::current().id();
        {
            let mut affinity = self.affinity.lock();
            match affinity.owner {
                None => return Err(ThreadAffineError::Unbound),
                Some(owner) if owner != current => return Err(ThreadAffineError::WrongThread),
                Some(_) if affinity.binding != access.binding => {
                    return Err(ThreadAffineError::StaleAccess);
                }
                Some(_) => {}
            }
            if affinity.modifying {
                return Err(ThreadAffineError::ReentrantAccess);
            }
            affinity.modifying = true;
        }
        // Construct the guard before touching TLS so an unavailable/reentrant
        // TLS borrow or panic relinquishes the claim on every failure path.
        let claim = BindingClaim {
            access: ThreadAffineAccess {
                slot: self,
                binding: access.binding,
                _not_send_or_sync: PhantomData,
            },
        };
        let slot_id = self.id();
        let occupied =
            with_values(|values| Ok(values.iter().any(|entry| entry.slot_id == slot_id)))?;
        Ok(if occupied {
            BindingState::Occupied(BindingTxn {
                claim,
                _state: PhantomData,
            })
        } else {
            BindingState::Empty(BindingTxn {
                claim,
                _state: PhantomData,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::assert_not_impl_any;

    assert_not_impl_any!(BindingTxn<'static, usize, BindingEmpty>: Send, Sync, Clone, Copy);
    assert_not_impl_any!(BindingTxn<'static, usize, BindingOccupied>: Send, Sync, Clone, Copy);

    #[test]
    fn miri_binding_claim_excludes_sibling_access_until_consumed() {
        let slot = ThreadAffineSlot::<usize>::new();
        let first = slot.bind_current().unwrap();
        let sibling = slot.bind_current().unwrap();
        let BindingState::Empty(empty) = slot.binding(&first).unwrap() else {
            panic!("new binding is empty")
        };
        assert!(matches!(
            slot.binding(&sibling),
            Err(ThreadAffineError::ReentrantAccess)
        ));
        let occupied = empty.install(7).unwrap();
        slot.with_mut(&sibling, |value| *value += 1).unwrap();
        assert!(matches!(
            slot.binding(&sibling),
            Err(ThreadAffineError::ReentrantAccess)
        ));
        let (value, empty) = occupied.take().unwrap();
        assert_eq!(value, 8);
        empty.release().unwrap();
        assert!(matches!(
            slot.with_mut(&first, |_| ()),
            Err(ThreadAffineError::Unbound)
        ));
    }

    #[test]
    fn miri_reentrant_take_returns_the_same_occupied_transaction() {
        let slot = ThreadAffineSlot::<usize>::new();
        let access = slot.bind_current().unwrap();
        let BindingState::Empty(empty) = slot.binding(&access).unwrap() else {
            panic!("new binding is empty")
        };
        let occupied = empty.install(11).unwrap();
        let error = slot
            .with_mut(&access, |_| occupied.take())
            .unwrap()
            .unwrap_err();
        assert_eq!(error.reason, ThreadAffineError::ReentrantAccess);
        let (value, empty) = error.transaction.take().unwrap();
        assert_eq!(value, 11);
        empty.release().unwrap();
    }

    #[test]
    fn miri_abandoned_binding_transaction_retains_tls_payload() {
        use std::sync::{Arc, atomic::AtomicUsize};
        struct CountDrop(Arc<AtomicUsize>);
        impl Drop for CountDrop {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let slot = ThreadAffineSlot::<CountDrop>::new();
        let access = slot.bind_current().unwrap();
        let BindingState::Empty(empty) = slot.binding(&access).unwrap() else {
            panic!("new binding is empty")
        };
        let occupied = empty
            .install(CountDrop(Arc::clone(&drops)))
            .unwrap_or_else(|_| panic!("install"));
        drop(occupied);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        let BindingState::Occupied(occupied) = slot.binding(&access).unwrap() else {
            panic!("payload retained")
        };
        let (value, empty) = occupied.take().unwrap();
        drop(value);
        empty.release().unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn forgotten_binding_transaction_keeps_its_exclusive_claim() {
        let mut slot = ThreadAffineSlot::<usize>::new();
        let access = slot.bind_current().unwrap();
        let BindingState::Empty(empty) = slot.binding(&access).unwrap() else {
            panic!("new binding is empty")
        };
        #[allow(
            clippy::mem_forget,
            reason = "regression: forgotten authority must block later occupancy changes"
        )]
        std::mem::forget(empty);
        assert!(matches!(
            slot.binding(&access),
            Err(ThreadAffineError::ReentrantAccess)
        ));
        // Exclusive test cleanup: no payload was installed and the forgotten
        // reference can never run again. Production has no claim-reset API.
        slot.affinity.get_mut().modifying = false;
        let access = slot.bind_current().unwrap();
        let BindingState::Empty(empty) = slot.binding(&access).unwrap() else {
            panic!("still empty")
        };
        empty.release().unwrap();
    }
    #[test]
    fn miri_stale_access_and_foreign_slot_are_rejected_on_acquisition() {
        let slot = ThreadAffineSlot::<usize>::new();
        let foreign = ThreadAffineSlot::<usize>::new();
        let old = slot.bind_current().unwrap();
        assert!(matches!(
            foreign.binding(&old),
            Err(ThreadAffineError::StaleAccess)
        ));
        let BindingState::Empty(empty) = old.binding().unwrap() else {
            panic!("expected empty")
        };
        empty.release().unwrap();
        assert!(matches!(old.binding(), Err(ThreadAffineError::Unbound)));
        let current = slot.bind_current().unwrap();
        assert!(matches!(old.binding(), Err(ThreadAffineError::StaleAccess)));
        assert_eq!(old.has_value(), Err(ThreadAffineError::StaleAccess));
        let BindingState::Empty(empty) = current.binding().unwrap() else {
            panic!("expected empty")
        };
        empty.release().unwrap();
    }

    #[test]
    fn miri_tls_reentry_rolls_back_acquisition_and_preserves_transition_value() {
        let first = ThreadAffineSlot::<usize>::new();
        let other = ThreadAffineSlot::<usize>::new();
        let access = first.bind_current().unwrap();
        let second = other.bind_current().unwrap();
        let BindingState::Empty(empty) = access.binding().unwrap() else {
            panic!("empty")
        };
        drop(empty.install(1).unwrap());
        let BindingState::Empty(second_empty) = second.binding().unwrap() else {
            panic!("empty")
        };
        let err = access
            .with_mut(|_| {
                assert!(matches!(
                    access.binding(),
                    Err(ThreadAffineError::ReentrantAccess)
                ));
                second_empty.install(42).unwrap_err()
            })
            .unwrap();
        assert_eq!(err.reason, ThreadAffineError::ReentrantAccess);
        assert_eq!(err.value, 42);
        let occupied = err.transaction.install(err.value).unwrap();
        let (value, empty) = occupied.take().unwrap();
        assert_eq!(value, 42);
        empty.release().unwrap();
        let BindingState::Occupied(txn) = access.binding().unwrap() else {
            panic!("occupied")
        };
        let (_, empty) = txn.take().unwrap();
        empty.release().unwrap();
    }

    #[test]
    fn miri_reentrant_empty_release_returns_transaction() {
        let first = ThreadAffineSlot::<usize>::new();
        let second = ThreadAffineSlot::<usize>::new();
        let access = first.bind_current().unwrap();
        let other = second.bind_current().unwrap();
        let BindingState::Empty(empty) = access.binding().unwrap() else {
            panic!("empty")
        };
        drop(empty.install(1).unwrap());
        let BindingState::Empty(empty) = other.binding().unwrap() else {
            panic!("empty")
        };
        let Err(err) = access.with_mut(|_| empty.release()).unwrap() else {
            panic!("TLS reentry must fail")
        };
        assert_eq!(err.reason, ThreadAffineError::ReentrantAccess);
        err.transaction.release().unwrap();
        let BindingState::Occupied(txn) = access.binding().unwrap() else {
            panic!("occupied")
        };
        let (_, empty) = txn.take().unwrap();
        empty.release().unwrap();
    }
    #[test]
    fn miri_rebind_is_blocked_between_release_and_claim_drop() {
        let slot = ThreadAffineSlot::<usize>::new();
        let access = slot.bind_current().unwrap();
        let BindingState::Empty(txn) = access.binding().unwrap() else {
            panic!("empty")
        };
        txn.claim.release_empty_claimed().unwrap();
        std::thread::scope(|scope| {
            assert_eq!(
                scope
                    .spawn(|| slot.bind_current().map(|_| ()))
                    .join()
                    .unwrap(),
                Err(ThreadAffineError::ReentrantAccess)
            );
        });
        drop(txn);
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let access = slot.bind_current().unwrap();
                    let BindingState::Empty(empty) = access.binding().unwrap() else {
                        panic!("empty")
                    };
                    empty.release().unwrap();
                })
                .join()
                .unwrap();
        });
    }

    #[test]
    fn miri_forgotten_occupied_claim_keeps_payload_and_exclusion() {
        let mut slot = ThreadAffineSlot::<usize>::new();
        let access = slot.bind_current().unwrap();
        let BindingState::Empty(txn) = access.binding().unwrap() else {
            panic!("empty")
        };
        let txn = txn.install(7).unwrap();
        #[allow(
            clippy::mem_forget,
            reason = "verify forgotten occupied claims retain payload and exclusivity"
        )]
        std::mem::forget(txn);
        assert_eq!(access.with_mut(|v| *v).unwrap(), 7);
        assert!(matches!(
            access.binding(),
            Err(ThreadAffineError::ReentrantAccess)
        ));
        // Test-only recovery: the forgotten token cannot be used or dropped.
        slot.affinity.get_mut().modifying = false;
        let access = slot.bind_current().unwrap();
        let BindingState::Occupied(txn) = access.binding().unwrap() else {
            panic!("occupied")
        };
        let (value, empty) = txn.take().unwrap();
        assert_eq!(value, 7);
        empty.release().unwrap();
    }
}
