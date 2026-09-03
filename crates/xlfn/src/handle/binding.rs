//! Formula-binding ownership and non-owning read-side publication.
//!
//! Every binding record is uniquely owned by this table and retained as a
//! tombstone until service reclamation. Atomic publication exposes only a
//! pointer; a per-record operation permit protects the binding's object
//! capability while a call reads it.

#![allow(
    unsafe_code,
    reason = "binding reads use audited non-owning pointers protected by per-record drain gates"
)]

use super::domain::{HandleDomainPermit, HandleReadDomain};
use super::object::{ObjectBinding, ObjectCell};
use super::token::HandleId;
use crate::error::DomainErrorCode;
use crate::generation::BindingGeneration;
use crate::{XllError, XllResult};
use parking_lot::{Mutex, RwLock, RwLockWriteGuard};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, AtomicU8, Ordering};

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BindingState {
    Live = 0,
    Retired = 1,
}

impl BindingState {
    pub(crate) fn from_raw(raw: u8) -> Self {
        match raw {
            value if value == Self::Live as u8 => Self::Live,
            value if value == Self::Retired as u8 => Self::Retired,
            _ => Self::Retired,
        }
    }
}

pub(crate) struct BindingRecord {
    pub(crate) id: HandleId,
    cell: NonNull<ObjectCell>,
    object: Mutex<Option<ObjectBinding>>,
    pub(crate) state: AtomicU8,
}

// SAFETY: BindingRecord is uniquely owned by BindingTable and transferred across threads.
unsafe impl Send for BindingRecord {}
// SAFETY: BindingRecord is immutable during publication and thread-safe.
unsafe impl Sync for BindingRecord {}

impl BindingRecord {
    fn new(id: HandleId, object: ObjectBinding) -> Self {
        let cell = NonNull::from(object.object());
        Self {
            id,
            cell,
            object: Mutex::new(Some(object)),
            state: AtomicU8::new(BindingState::Live as u8),
        }
    }

    pub(crate) fn state(&self) -> BindingState {
        BindingState::from_raw(self.state.load(Ordering::Acquire))
    }

    #[inline(always)]
    pub(crate) fn object(&self) -> &ObjectCell {
        // SAFETY: callers are protected by the HandleReadDomain, which ensures
        // the ObjectCell cannot be freed until all in-flight readers finish.
        unsafe { self.cell.as_ref() }
    }

    fn duplicate_object_binding(&self) -> XllResult<ObjectBinding> {
        self.object
            .lock()
            .as_ref()
            .ok_or(XllError::StaleHandle)?
            .duplicate()
    }

    fn retire(&self) -> ObjectBinding {
        self.object
            .lock()
            .take()
            .expect("binding retirement consumes one object capability")
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct BindingPtr(NonNull<BindingRecord>);

impl BindingPtr {
    fn from_ref(record: &BindingRecord) -> Self {
        Self(NonNull::from(record))
    }

    fn get(self) -> &'static BindingRecord {
        // SAFETY: binding records are table-owned tombstones and are not
        // reclaimed until every publication/read has been withdrawn.
        unsafe { self.0.as_ref() }
    }
}

// SAFETY: BindingPtr is an audited pointer to an immutable BindingRecord whose
// lifetime is guaranteed by the table-quiescence protocol.
unsafe impl Send for BindingPtr {}
// SAFETY: BindingRecord is thread-safe and immutable borrows can be shared.
unsafe impl Sync for BindingPtr {}

pub(crate) struct BindingSnapshot {
    record: Option<BindingPtr>,
}

/// A call-scoped capability that prevents one binding's object reference from
/// being retired while it is projected into a typed handle.
pub(crate) struct BindingReadLease {
    record: BindingPtr,
    _permit: Option<HandleDomainPermit>,
}

impl BindingReadLease {
    #[cfg(test)]
    pub(crate) fn new(
        snapshot: BindingSnapshot,
        id: HandleId,
        domain: &HandleReadDomain,
    ) -> XllResult<Self> {
        let permit = domain.enter()?;
        let record = snapshot.record.ok_or(XllError::StaleHandle)?;
        let record_ref = record.get();
        if record_ref.id != id || record_ref.state() != BindingState::Live {
            return Err(XllError::StaleHandle);
        }
        Ok(Self {
            record,
            _permit: Some(permit),
        })
    }

    #[inline]
    pub(crate) fn new_scoped(snapshot: BindingSnapshot, id: HandleId) -> XllResult<Self> {
        let record = snapshot.record.ok_or(XllError::StaleHandle)?;
        let record_ref = record.get();
        if record_ref.id != id || record_ref.state() != BindingState::Live {
            return Err(XllError::StaleHandle);
        }
        Ok(Self {
            record,
            _permit: None,
        })
    }

    pub(crate) fn record(&self) -> &BindingRecord {
        self.record.get()
    }

    pub(crate) fn object(&self) -> &ObjectCell {
        self.record().object()
    }

    pub(crate) fn duplicate_object_binding(&self) -> XllResult<ObjectBinding> {
        self.record().duplicate_object_binding()
    }

    pub(crate) fn acquire_object_lease(&self) -> XllResult<super::object::ObjectLeaseGuard> {
        self.record()
            .object
            .lock()
            .as_ref()
            .ok_or(XllError::StaleHandle)?
            .acquire_lease()
    }
}

pub(crate) struct PublishedBindings {
    entries: Box<[AtomicPtr<BindingRecord>]>,
}

impl PublishedBindings {
    pub(crate) fn new(maximum_bindings: u32) -> Self {
        Self {
            entries: (0..maximum_bindings.max(1))
                .map(|_| AtomicPtr::new(std::ptr::null_mut()))
                .collect(),
        }
    }

    pub(crate) fn load(&self, slot: u32) -> BindingSnapshot {
        let record = self
            .entries
            .get(slot as usize)
            .and_then(|entry| NonNull::new(entry.load(Ordering::Acquire)))
            .map(BindingPtr);
        BindingSnapshot { record }
    }

    fn insert(&self, id: HandleId, record: BindingPtr) {
        let Some(entry) = self.entries.get(id.slot as usize) else {
            xlfn_kernel::invariant::fail_stop();
        };
        if !entry.load(Ordering::Acquire).is_null() {
            xlfn_kernel::invariant::fail_stop();
        }
        entry.store(record.0.as_ptr(), Ordering::Release);
    }

    fn remove(&self, id: HandleId, expected: BindingPtr) {
        let Some(entry) = self.entries.get(id.slot as usize) else {
            return;
        };
        let _ = entry.compare_exchange(
            expected.0.as_ptr(),
            std::ptr::null_mut(),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn clear(&self) {
        for entry in &self.entries {
            entry.store(std::ptr::null_mut(), Ordering::Release);
        }
    }
}

pub(crate) struct BindingSlot {
    pub(crate) next_generation: BindingGeneration,
    pub(crate) record: Option<BindingPtr>,
}

pub(crate) struct RegistryState {
    pub(crate) slots: Vec<BindingSlot>,
    pub(crate) free: Vec<usize>,
    pub(crate) live_bindings: u32,
    #[allow(
        clippy::vec_box,
        reason = "BindingRecord requires stable heap addresses for non-owning BindingPtr"
    )]
    records: Vec<Box<BindingRecord>>,
}

pub(crate) struct BindingTable {
    state: RwLock<RegistryState>,
    published: PublishedBindings,
    read_domain: HandleReadDomain,
    maximum_bindings: u32,
}

impl BindingTable {
    pub(crate) fn new(maximum_bindings: u32) -> Self {
        Self {
            state: RwLock::new(RegistryState {
                slots: Vec::new(),
                free: Vec::new(),
                live_bindings: 0,
                records: Vec::new(),
            }),
            published: PublishedBindings::new(maximum_bindings),
            read_domain: HandleReadDomain::new(),
            maximum_bindings,
        }
    }

    pub(crate) fn read_domain(&self) -> &HandleReadDomain {
        &self.read_domain
    }

    pub(crate) fn reserve(&self) -> XllResult<BindingReservation<'_>> {
        let mut state = self.state.write();
        if state.live_bindings >= self.maximum_bindings {
            return Err(XllError::Domain {
                code: DomainErrorCode::Overflow,
            });
        }
        let (index, slot, reused, appended) = match state.free.pop() {
            Some(index) => {
                let slot = u32::try_from(index).map_err(|_| XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_SLOT,
                })?;
                (index, slot, true, false)
            }
            None => {
                let index = state.slots.len();
                let slot = u32::try_from(index).map_err(|_| XllError::Domain {
                    code: DomainErrorCode::Overflow,
                })?;
                state.slots.push(BindingSlot {
                    next_generation: BindingGeneration::ONE,
                    record: None,
                });
                (index, slot, false, true)
            }
        };
        let id = HandleId {
            slot,
            generation: state.slots[index].next_generation,
        };
        Ok(BindingReservation {
            table: self,
            state: Some(state),
            index,
            id,
            reused,
            appended,
            active: true,
        })
    }

    #[cfg(test)]
    pub(crate) fn read_state(&self) -> parking_lot::RwLockReadGuard<'_, RegistryState> {
        self.state.read()
    }

    #[cfg(test)]
    pub(crate) fn try_read_state(&self) -> Option<parking_lot::RwLockReadGuard<'_, RegistryState>> {
        self.state.try_read()
    }

    #[cfg(test)]
    pub(crate) fn write_state(&self) -> parking_lot::RwLockWriteGuard<'_, RegistryState> {
        self.state.write()
    }

    pub(crate) fn published(&self) -> &PublishedBindings {
        &self.published
    }

    pub(crate) fn begin_removal(&self, id: HandleId) -> XllResult<BindingRemoval<'_>> {
        let state = self.state.write();
        let record = state
            .slots
            .get(id.slot as usize)
            .and_then(|slot| slot.record)
            .filter(|record| record.get().id == id)
            .ok_or(XllError::StaleHandle)?;
        Ok(BindingRemoval {
            table: self,
            state: Some(state),
            id,
            record,
            active: true,
        })
    }

    pub(crate) fn retire_all(&self) -> (u32, Vec<ObjectBinding>) {
        let mut state = self.state.write();
        let live_bindings = state.live_bindings;
        let mut retired = Vec::with_capacity(live_bindings as usize);
        state.free.clear();
        self.published.clear();
        let mut records = Vec::with_capacity(live_bindings as usize);
        for index in 0..state.slots.len() {
            let reusable = {
                let slot = &mut state.slots[index];
                if let Some(record) = slot.record.take() {
                    record
                        .get()
                        .state
                        .store(BindingState::Retired as u8, Ordering::Release);
                    records.push(record);
                }
                if let Some(next) = slot.next_generation.next() {
                    slot.next_generation = next;
                    true
                } else {
                    false
                }
            };
            if reusable {
                state.free.push(index);
            }
        }
        state.live_bindings = 0;
        drop(state);
        self.read_domain.quiesce();
        for record in records {
            retired.push(record.get().retire());
        }
        (live_bindings, retired)
    }
}

pub(crate) struct BindingReservation<'table> {
    pub(super) table: &'table BindingTable,
    pub(super) state: Option<RwLockWriteGuard<'table, RegistryState>>,
    pub(super) index: usize,
    pub(super) id: HandleId,
    pub(super) reused: bool,
    pub(super) appended: bool,
    pub(super) active: bool,
}

impl BindingReservation<'_> {
    pub(crate) fn publish(mut self, object: ObjectBinding) -> (HandleId, bool) {
        let mut state = self
            .state
            .take()
            .expect("binding reservation owns the table write lock");
        let record = Box::new(BindingRecord::new(self.id, object));
        let pointer = BindingPtr::from_ref(record.as_ref());
        state.records.push(record);
        state.slots[self.index].record = Some(pointer);
        self.table.published.insert(self.id, pointer);
        state.live_bindings = state
            .live_bindings
            .checked_add(1)
            .expect("binding capacity was checked before commit");
        self.active = false;
        drop(state);
        (self.id, self.reused)
    }
}

impl Drop for BindingReservation<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .state
            .take()
            .expect("binding reservation owns the table write lock");
        if self.appended {
            let slot = state.slots.pop().expect("reservation owns the final slot");
            debug_assert!(slot.record.is_none());
        } else if self.reused {
            state.free.push(self.index);
        }
    }
}

pub(crate) struct BindingRemoval<'table> {
    pub(super) table: &'table BindingTable,
    pub(super) state: Option<RwLockWriteGuard<'table, RegistryState>>,
    pub(super) id: HandleId,
    pub(super) record: BindingPtr,
    pub(super) active: bool,
}

impl BindingRemoval<'_> {
    #[cfg(test)]
    pub(crate) fn object(&self) -> &ObjectCell {
        self.record.get().object()
    }

    pub(crate) fn commit(mut self) -> bool {
        let mut state = self
            .state
            .take()
            .expect("binding removal owns the table write lock");
        let record = self.record.get();
        record
            .state
            .store(BindingState::Retired as u8, Ordering::Release);
        self.table.published.remove(self.id, self.record);
        let slot = state
            .slots
            .get_mut(self.id.slot as usize)
            .expect("binding slot was validated");
        let slot_record = slot.record.take().expect("binding record was validated");
        if slot_record != self.record {
            xlfn_kernel::invariant::fail_stop();
        }
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
            state.free.push(self.id.slot as usize);
        }
        self.active = false;
        drop(state);
        self.table.read_domain.quiesce();
        drop(record.retire());
        reusable
    }
}

impl Drop for BindingRemoval<'_> {
    fn drop(&mut self) {
        debug_assert_eq!(self.state.is_some(), self.active);
    }
}
