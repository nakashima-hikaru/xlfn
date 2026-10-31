//! Exclusive publication claims and protected pre-grace ownership.
use super::*;
use crate::handle::domain::UnlinkedBindingOwner;
use std::mem::ManuallyDrop;

/// A slot retains this owner even on unwind or abandonment. Ordinary Drop
/// must never recover an allocation that raw readers can still access.
pub(crate) struct PublishedBindingOwner {
    owner: ManuallyDrop<PublishedOwner<BindingRecord>>,
}
impl AsRef<BindingRecord> for PublishedBindingOwner {
    fn as_ref(&self) -> &BindingRecord {
        self.owner.as_ref()
    }
}
impl std::ops::Deref for PublishedBindingOwner {
    type Target = BindingRecord;
    fn deref(&self) -> &BindingRecord {
        self.as_ref()
    }
}

enum ReservationOrigin {
    Reused,
    Appended,
}

pub(crate) struct BindingReservation<'table> {
    table: &'table BindingTable,
    state: Option<RwLockWriteGuard<'table, RegistryState>>,
    index: usize,
    id: HandleId,
    origin: ReservationOrigin,
    active: bool,
}

impl<'table> BindingReservation<'table> {
    pub(super) fn acquire(table: &'table BindingTable) -> XllResult<Self> {
        let mut state = table.state.write();
        // A remover may itself hold a call permit and must not wait for it.
        // Stop new publication at hard debt even in that case. With the live
        // binding cap this bounds total records by maximum_bindings + 256,
        // including batches whose arbitrary destructors are still running.
        if table.read_domain.debt() >= crate::handle::domain::HARD_DEBT_LIMIT {
            return Err(XllError::Overloaded);
        }
        if state.live_bindings >= table.maximum_bindings {
            return Err(XllError::Domain {
                code: DomainErrorCode::Overflow,
            });
        }
        let (index, slot, origin) = match state.free.pop() {
            Some(slot) => (slot as usize, slot, ReservationOrigin::Reused),
            None => {
                let index = state.slots.len();
                // Exhausted generations permanently consume their slot. Do
                // not treat padding in the last publication page as capacity.
                if index >= table.maximum_bindings as usize {
                    return Err(XllError::Domain {
                        code: DomainErrorCode::Overflow,
                    });
                }
                let slot = u32::try_from(index).map_err(|_| XllError::Domain {
                    code: DomainErrorCode::Overflow,
                })?;
                state.slots.push(BindingSlot {
                    next_generation: BindingGeneration::ONE,
                    record: None,
                });
                (index, slot, ReservationOrigin::Appended)
            }
        };
        let id = HandleId {
            slot,
            generation: state.slots[index].next_generation,
        };
        Ok(BindingReservation {
            table,
            state: Some(state),
            index,
            id,
            origin,
            active: true,
        })
    }

    pub(crate) fn id(&self) -> HandleId {
        self.id
    }

    pub(crate) fn publish(mut self, object: ObjectBinding) -> (HandleId, bool) {
        let state = self
            .state
            .as_mut()
            .expect("binding reservation owns the table write lock");
        let record = PublishedBindingOwner {
            owner: ManuallyDrop::new(PublishedOwner::new(BindingRecord::new(self.id, object))),
        };
        let slot = &mut state.slots[self.index];
        slot.record = Some(record);
        self.active = false;
        let pointer = BindingPtr::from_ref(slot.record.as_ref().unwrap().as_ref());
        self.table
            .publication_writer(state)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop())
            .insert(self.id, pointer);
        state.live_bindings = state
            .live_bindings
            .checked_add(1)
            .expect("binding capacity was checked before commit");
        self.active = false;
        drop(self.state.take());
        (self.id(), matches!(self.origin, ReservationOrigin::Reused))
    }
}

impl Drop for BindingReservation<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let state = self
            .state
            .as_mut()
            .expect("binding reservation owns the table write lock");
        if matches!(self.origin, ReservationOrigin::Appended) {
            let slot = state.slots.pop().expect("reservation owns the final slot");
            debug_assert!(slot.record.is_none());
        } else {
            state.free.push(self.id.slot);
        }
    }
}

pub(crate) struct BindingRemoval<'table> {
    table: &'table BindingTable,
    state: Option<RwLockWriteGuard<'table, RegistryState>>,
    id: HandleId,
    record: BindingPtr,
    active: bool,
}

impl<'table> BindingRemoval<'table> {
    pub(super) fn acquire(table: &'table BindingTable, id: HandleId) -> XllResult<Self> {
        let state = table.state.write();
        let record = state
            .slots
            .get(id.slot as usize)
            .and_then(|slot| slot.record.as_deref())
            .filter(|record| record.id == id)
            .map(BindingPtr::from_ref)
            .ok_or(XllError::StaleHandle)?;
        Ok(BindingRemoval {
            table,
            state: Some(state),
            id,
            record,
            active: true,
        })
    }

    #[cfg(test)]
    pub(crate) fn object(&self) -> &ObjectCell {
        // SAFETY: self owns the table write lock and the slot holds the live record.
        unsafe { self.record.0.as_ref() }.object()
    }

    pub(crate) fn commit(mut self) -> bool {
        let state = self
            .state
            .as_mut()
            .expect("binding removal owns the table write lock");
        let slot = state
            .slots
            .get_mut(self.id.slot as usize)
            .expect("binding slot was validated");
        let retired = slot.record.take().expect("binding record was validated");
        if BindingPtr::from_ref(retired.as_ref()) != self.record {
            xlfn_kernel::invariant::fail_stop();
        }
        retired
            .state
            .store(BindingState::Retired as u8, Ordering::Release);
        self.table
            .publication_writer(state)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop())
            .remove(self.id, self.record);
        let slot = &mut state.slots[self.id.slot as usize];
        let reusable = if let Some(next) = slot.next_generation.next() {
            slot.next_generation = next;
            true
        } else {
            false
        };
        state.live_bindings = state
            .live_bindings
            .checked_sub(1)
            .expect("binding removal cannot underflow");
        if reusable {
            state.free.push(self.id.slot);
        }
        // Enqueue before releasing the table lock: seal's retire_all must
        // not pass this removal and drain the domain before it is registered.
        self.table
            .read_domain
            .enqueue_reclaim(UnlinkedBindingOwner::retain(retired.owner));
        drop(self.state.take());
        self.active = false;
        self.table.read_domain.maintain_after_removal();
        reusable
    }
}

impl Drop for BindingRemoval<'_> {
    fn drop(&mut self) {
        debug_assert_eq!(self.state.is_some(), self.active);
    }
}

impl Drop for BindingTable {
    fn drop(&mut self) {
        // All reclamation runs under a borrowing call/writer capability, and
        // its final access must finish before the unique domain is destroyed.
        self.read_domain.seal();
        let state = self.state.get_mut();
        for slot in &mut state.slots {
            if let Some(record) = slot.record.take() {
                drop(ManuallyDrop::into_inner(record.owner));
            }
        }
    }
}

/// Holds withdrawn owners while the exact table's grace period is pending.
struct BulkRetirement<'table> {
    table: &'table BindingTable,
    state: Option<RwLockWriteGuard<'table, RegistryState>>,
    records: Vec<PublishedBindingOwner>,
    live_bindings: u32,
}

impl<'table> BulkRetirement<'table> {
    fn withdraw(table: &'table BindingTable) -> Self {
        Self::withdraw_with(table, || {})
    }

    fn withdraw_with(table: &'table BindingTable, mut after_unlink: impl FnMut()) -> Self {
        let mut state = table.state.write();
        let live_bindings = state.live_bindings;
        // Allocate before the first unlink. Every owner already suppresses
        // payload Drop, including partial construction and unwinding.
        let records = Vec::with_capacity(live_bindings as usize);
        let additional_free = state.slots.len().saturating_sub(state.free.len());
        state.free.reserve(additional_free);
        let mut transaction = Self {
            table,
            state: Some(state),
            records,
            live_bindings,
        };
        let state = transaction
            .state
            .as_mut()
            .expect("bulk retirement holds its writer guard");
        state.free.clear();
        for index in 0..state.slots.len() {
            if let Some(record) = state.slots[index].record.take() {
                table
                    .publication_writer(state)
                    .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop())
                    .remove(record.id, BindingPtr::from_ref(&record));
                record
                    .state
                    .store(BindingState::Retired as u8, Ordering::Release);
                transaction.records.push(record);
                after_unlink();
            }
            let slot = &mut state.slots[index];
            if let Some(next) = slot.next_generation.next() {
                slot.next_generation = next;
                state
                    .free
                    .push(u32::try_from(index).expect("allocated slot fits u32"));
            }
        }
        state.live_bindings = 0;
        transaction
    }

    fn wait(mut self) -> (u32, BulkBindingsDrained) {
        drop(self.state.take());
        self.table.read_domain.quiesce();
        (
            self.live_bindings,
            BulkBindingsDrained {
                records: self.records,
            },
        )
    }
}

/// Only BulkRetirement::wait can authorize destruction of these owners.
/// No owner array, Box recovery, or mutable payload access escapes this module.
pub(crate) struct BulkBindingsDrained {
    records: Vec<PublishedBindingOwner>,
}
impl Drop for BulkBindingsDrained {
    fn drop(&mut self) {
        for record in self.records.drain(..) {
            drop(ManuallyDrop::into_inner(record.owner));
        }
    }
}

pub(super) fn retire_all(table: &BindingTable) -> (u32, BulkBindingsDrained) {
    BulkRetirement::withdraw(table).wait()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::registry::HandleRegistry;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    struct Counted(Arc<AtomicUsize>);
    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Release);
        }
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "exercise safe abandonment of the full bulk capability"
    )]
    #[cfg_attr(
        miri,
        ignore = "intentionally retains the transaction, writer guard and registry"
    )]
    fn forgotten_bulk_retirement_cannot_destroy_live_reader_payload() {
        let registry = HandleRegistry::from_entropy(1, [7; 40]);
        let drops = Arc::new(AtomicUsize::new(0));
        let token = registry
            .insert_pending(&mut Some(Counted(Arc::clone(&drops))))
            .unwrap();
        let id = registry
            .codec
            .parse(
                std::ptr::from_ref(&registry).addr(),
                crate::handle::token::HandleToken::new(&token),
            )
            .unwrap()
            .id;
        let reader = registry.bindings.read_standalone(id).unwrap();
        let retirement = BulkRetirement::withdraw(&registry.bindings);
        std::mem::forget(retirement);
        assert_eq!(reader.record().id, id);
        assert_eq!(reader.record().state(), BindingState::Retired);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert!(
            registry.bindings.try_read_state().is_none(),
            "forgotten writer blocks progress rather than certifying grace"
        );
        drop(reader);
        std::mem::forget(registry);
    }

    #[test]
    fn miri_abandoned_bulk_retirement_retains_live_reader_payload() {
        let registry = HandleRegistry::from_entropy(1, [7; 40]);
        let drops = Arc::new(AtomicUsize::new(0));
        let token = registry
            .insert_pending(&mut Some(Counted(Arc::clone(&drops))))
            .unwrap();
        let id = registry
            .codec
            .parse(
                std::ptr::from_ref(&registry).addr(),
                crate::handle::token::HandleToken::new(&token),
            )
            .unwrap()
            .id;
        let reader = registry.bindings.read_standalone(id).unwrap();
        let retirement = BulkRetirement::withdraw(&registry.bindings);
        let pointer = retirement.records[0].owner.as_ptr();
        drop(retirement);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert_eq!(reader.record().id, id);
        assert_eq!(reader.record().state(), BindingState::Retired);
        assert!(registry.bindings.published.load(id.slot).record.is_none());
        assert!(registry.bindings.try_read_state().is_some());
        assert_eq!(
            registry.bindings.read_domain().debt(),
            0,
            "bulk does not charge the deferred queue"
        );
        drop(reader);
        registry.bindings.read_domain().quiesce();
        // SAFETY: test-only recovery of the allocation deliberately retained
        // by the abandoned non-dropping owner. The exact domain has drained;
        // the slot and transaction no longer own it and no reader remains.
        unsafe {
            drop(Box::from_raw(pointer));
        }
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn miri_bulk_unlink_panic_cannot_destroy_before_grace() {
        let registry = HandleRegistry::from_entropy(1, [7; 40]);
        let drops = Arc::new(AtomicUsize::new(0));
        let token = registry
            .insert_pending(&mut Some(Counted(Arc::clone(&drops))))
            .unwrap();
        let id = registry
            .codec
            .parse(
                std::ptr::from_ref(&registry).addr(),
                crate::handle::token::HandleToken::new(&token),
            )
            .unwrap()
            .id;
        let reader = registry.bindings.read_standalone(id).unwrap();
        let pointer = registry.bindings.state.read().slots[0]
            .record
            .as_ref()
            .unwrap()
            .owner
            .as_ptr();
        let failure = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            BulkRetirement::withdraw_with(&registry.bindings, || panic!("after bulk unlink"))
        }));
        assert!(failure.is_err());
        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert_eq!(reader.record().id, id);
        assert_eq!(reader.record().state(), BindingState::Retired);
        assert!(registry.bindings.try_read_state().is_some());
        drop(reader);
        registry.bindings.read_domain().quiesce();
        // SAFETY: same test-only retained-allocation recovery as above, after
        // unwinding released the writer guard and the exact domain drained.
        unsafe {
            drop(Box::from_raw(pointer));
        }
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn miri_bulk_batch_destroys_once_after_writer_and_domain_unlock() {
        struct Reenter {
            registry: std::sync::Weak<HandleRegistry>,
            drops: Arc<AtomicUsize>,
        }
        impl Drop for Reenter {
            fn drop(&mut self) {
                let registry = self.registry.upgrade().unwrap();
                assert!(registry.bindings.try_read_state().is_some());
                // This acquires the transition/queue locks; user destruction
                // must be able to reenter after the outer grace period ends.
                registry.bindings.read_domain().quiesce();
                self.drops.fetch_add(1, Ordering::Release);
            }
        }
        let registry = Arc::new(HandleRegistry::from_entropy(2, [7; 40]));
        let drops = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            registry
                .insert_pending(&mut Some(Reenter {
                    registry: Arc::downgrade(&registry),
                    drops: Arc::clone(&drops),
                }))
                .unwrap();
        }
        let (count, drained) = registry.bindings.retire_all();
        assert_eq!(count, 2);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        drop(drained);
        assert_eq!(drops.load(Ordering::Acquire), 2);
        assert_eq!(registry.bindings.read_domain().debt(), 0);
    }

    #[test]
    fn miri_table_drop_reclaims_protected_live_slots() {
        let mut registry = HandleRegistry::from_entropy(1, [7; 40]);
        let drops = Arc::new(AtomicUsize::new(0));
        registry
            .insert_pending(&mut Some(Counted(Arc::clone(&drops))))
            .unwrap();
        // Keep the arena owner alive while dropping the table directly; this
        // bypasses registry retirement and checks the table's terminal path.
        let bindings = std::mem::replace(&mut registry.bindings, BindingTable::new(0));
        drop(bindings);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }
}
