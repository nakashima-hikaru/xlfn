//! Binding ownership and its immutable read-side publication.
//!
//! This module owns the formula-token side of the handle registry. A binding
//! record owns the shared [`ObjectCell`] reference that makes its immutable
//! publication snapshot a complete read-side lifetime proof.

use super::object::SharedObject;
use super::token::HandleId;
use crate::error::DomainErrorCode;
use crate::generation::BindingGeneration;
use crate::{XllError, XllResult};
use arc_swap::ArcSwapAny;
use parking_lot::{RwLock, RwLockWriteGuard};
use std::sync::atomic::{AtomicU8, Ordering};

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

/// One canonical formula-binding record shared by the mutable registry and
/// the immutable read-side publication snapshot.
pub(crate) struct BindingRecord {
    pub(crate) id: HandleId,
    pub(crate) object: SharedObject,
    pub(crate) state: AtomicU8,
}

impl BindingRecord {
    fn new(id: HandleId, object: SharedObject) -> Self {
        Self {
            id,
            object,
            state: AtomicU8::new(BindingState::Live as u8),
        }
    }

    pub(crate) fn state(&self) -> BindingState {
        BindingState::from_raw(self.state.load(Ordering::Acquire))
    }
}

const BINDING_CHUNK_SIZE: usize = 64;

#[derive(Clone)]
struct BindingChunk {
    entries: [Option<triomphe::Arc<BindingRecord>>; BINDING_CHUNK_SIZE],
}

impl BindingChunk {
    fn empty() -> Self {
        Self {
            entries: [const { None }; BINDING_CHUNK_SIZE],
        }
    }
}

pub(crate) struct BindingSnapshot {
    guard: arc_swap::Guard<triomphe::Arc<BindingChunk>>,
}

impl BindingSnapshot {
    pub(crate) fn get(&self, slot: u32) -> Option<&triomphe::Arc<BindingRecord>> {
        self.guard.entries[slot as usize & (BINDING_CHUNK_SIZE - 1)].as_ref()
    }
}

/// A call-scoped read capability. The snapshot transitively owns the binding
/// record and its `ObjectCell`; no object `Arc` is cloned on warm lookup.
pub(crate) struct BindingReadLease {
    snapshot: BindingSnapshot,
    id: HandleId,
}

impl BindingReadLease {
    pub(crate) fn new(snapshot: BindingSnapshot, id: HandleId) -> XllResult<Self> {
        let valid = snapshot.get(id.slot).is_some_and(|record| record.id == id);
        if !valid {
            return Err(XllError::StaleHandle);
        }
        Ok(Self { snapshot, id })
    }

    pub(crate) fn record(&self) -> &BindingRecord {
        self.snapshot
            .get(self.id.slot)
            .filter(|record| record.id == self.id)
            .map(triomphe::Arc::as_ref)
            .expect("validated binding read lease")
    }

    pub(crate) fn object(&self) -> &SharedObject {
        &self.record().object
    }
}

/// Immutable slot-indexed publication snapshots for warm handle lookup.
pub(crate) struct PublishedBindings {
    chunks: Box<[ArcSwapAny<triomphe::Arc<BindingChunk>>]>,
    empty: ArcSwapAny<triomphe::Arc<BindingChunk>>,
}

impl PublishedBindings {
    pub(crate) fn new(maximum_bindings: u32) -> Self {
        let chunk_count = (maximum_bindings as usize)
            .div_ceil(BINDING_CHUNK_SIZE)
            .max(1);
        let empty_chunk = triomphe::Arc::new(BindingChunk::empty());
        Self {
            chunks: (0..chunk_count)
                .map(|_| ArcSwapAny::new(triomphe::Arc::clone(&empty_chunk)))
                .collect(),
            empty: ArcSwapAny::new(empty_chunk),
        }
    }

    fn chunk_index(slot: u32) -> usize {
        slot as usize / BINDING_CHUNK_SIZE
    }

    /// Load the chunk containing one publication.
    pub(crate) fn load(&self, slot: u32) -> BindingSnapshot {
        let chunk = self
            .chunks
            .get(Self::chunk_index(slot))
            .unwrap_or(&self.empty);
        BindingSnapshot {
            guard: chunk.load(),
        }
    }

    /// Update the snapshot while the canonical registry write lock is held.
    fn insert(&self, id: HandleId, record: triomphe::Arc<BindingRecord>) {
        let slot = id.slot;
        let Some(chunk) = self.chunks.get(Self::chunk_index(slot)) else {
            debug_assert!(false, "handle slot exceeds the publication table");
            return;
        };
        let current = chunk.load_full();
        let mut next = current.as_ref().clone();
        next.entries[slot as usize & (BINDING_CHUNK_SIZE - 1)] = Some(record);
        chunk.store(triomphe::Arc::new(next));
    }

    /// Remove only the publication that belongs to the canonical entry being
    /// removed.
    fn remove(&self, id: HandleId, expected: &triomphe::Arc<BindingRecord>) {
        let slot = id.slot;
        let Some(chunk) = self.chunks.get(Self::chunk_index(slot)) else {
            return;
        };
        let current = chunk.load_full();
        if !current.entries[slot as usize & (BINDING_CHUNK_SIZE - 1)]
            .as_ref()
            .is_some_and(|record| triomphe::Arc::ptr_eq(record, expected))
        {
            return;
        }
        let mut next = current.as_ref().clone();
        next.entries[slot as usize & (BINDING_CHUNK_SIZE - 1)] = None;
        chunk.store(triomphe::Arc::new(next));
    }

    /// Clear all publication snapshots while the canonical registry is being
    /// closed.
    fn clear(&self) {
        let empty_chunk = triomphe::Arc::new(BindingChunk::empty());
        for chunk in &self.chunks {
            chunk.store(triomphe::Arc::clone(&empty_chunk));
        }
    }
}

pub(crate) struct BindingSlot {
    pub(crate) next_generation: BindingGeneration,
    pub(crate) record: Option<triomphe::Arc<BindingRecord>>,
}

pub(crate) struct RegistryState {
    pub(crate) slots: Vec<BindingSlot>,
    pub(crate) free: Vec<usize>,
    pub(crate) live_bindings: u32,
}

/// Canonical binding ownership and its immutable read-side publication.
pub(crate) struct BindingTable {
    state: RwLock<RegistryState>,
    published: PublishedBindings,
    maximum_bindings: u32,
}

impl BindingTable {
    pub(crate) fn new(maximum_bindings: u32) -> Self {
        Self {
            state: RwLock::new(RegistryState {
                slots: Vec::new(),
                free: Vec::new(),
                live_bindings: 0,
            }),
            published: PublishedBindings::new(maximum_bindings),
            maximum_bindings,
        }
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
                let slot = match u32::try_from(index) {
                    Ok(slot) => slot,
                    Err(_) => {
                        state.free.push(index);
                        return Err(XllError::Internal {
                            diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_SLOT,
                        });
                    }
                };
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
            .and_then(|slot| slot.record.as_ref())
            .filter(|record| record.id == id)
            .cloned()
            .ok_or(XllError::StaleHandle)?;
        Ok(BindingRemoval {
            table: self,
            state: Some(state),
            id,
            record,
            active: true,
        })
    }

    pub(crate) fn retire_all(&self) -> (u32, Vec<triomphe::Arc<BindingRecord>>) {
        let mut state = self.state.write();
        let live_bindings = state.live_bindings;
        let mut retired = Vec::with_capacity(live_bindings as usize);
        state.free.clear();
        self.published.clear();
        for index in 0..state.slots.len() {
            let reusable = {
                let slot = &mut state.slots[index];
                if let Some(record) = slot.record.take() {
                    record
                        .state
                        .store(BindingState::Retired as u8, Ordering::Release);
                    retired.push(record);
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
    pub(crate) fn publish(mut self, object: SharedObject) -> (HandleId, bool) {
        let mut state = self
            .state
            .take()
            .expect("binding reservation must own the table write lock");
        let record = triomphe::Arc::new(BindingRecord::new(self.id, object));
        state.slots[self.index].record = Some(triomphe::Arc::clone(&record));
        self.table.published.insert(self.id, record);
        state.live_bindings = state
            .live_bindings
            .checked_add(1)
            .expect("binding reservation capacity was checked before commit");
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
            .expect("binding reservation must own the table write lock");
        if self.appended {
            let slot = state
                .slots
                .pop()
                .expect("new binding reservation owns the final slot");
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
    pub(super) record: triomphe::Arc<BindingRecord>,
    pub(super) active: bool,
}

impl BindingRemoval<'_> {
    #[cfg(test)]
    pub(crate) fn object(&self) -> &SharedObject {
        &self.record.object
    }

    pub(crate) fn commit(mut self) -> bool {
        let mut state = self
            .state
            .take()
            .expect("binding removal must own the table write lock");
        self.record
            .state
            .store(BindingState::Retired as u8, Ordering::Release);
        self.table.published.remove(self.id, &self.record);
        let slot = state
            .slots
            .get_mut(self.id.slot as usize)
            .expect("binding slot was checked above");
        let slot_record = slot
            .record
            .take()
            .expect("binding record was checked above");
        let reusable = if let Some(next) = slot.next_generation.next() {
            slot.next_generation = next;
            true
        } else {
            false
        };
        state.live_bindings = state
            .live_bindings
            .checked_sub(1)
            .expect("binding removal cannot underflow live count");
        if reusable {
            state.free.push(self.id.slot as usize);
        }
        self.active = false;
        drop(state);
        // The binding lock must never be held while the final `ObjectCell`
        // reference runs arbitrary user `Drop` code.
        drop(slot_record);
        reusable
    }
}

impl Drop for BindingRemoval<'_> {
    fn drop(&mut self) {
        debug_assert_eq!(self.state.is_some(), self.active);
    }
}
