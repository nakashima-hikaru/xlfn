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
        match self.claim.access.slot.install(&self.claim.access, value) {
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
        match self
            .claim
            .access
            .slot
            .release_empty_binding(&self.claim.access)
        {
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
        match self.claim.access.slot.take(&self.claim.access) {
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
        self.verify_access(access)?;
        let mut affinity = self.affinity.lock();
        if affinity.modifying {
            return Err(ThreadAffineError::ReentrantAccess);
        }
        let slot_id = self.id();
        let occupied =
            with_values(|values| Ok(values.iter().any(|entry| entry.slot_id == slot_id)))?;
        affinity.modifying = true;
        let claim = BindingClaim {
            access: ThreadAffineAccess {
                slot: self,
                binding: access.binding,
                _not_send_or_sync: PhantomData,
            },
        };
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
}
