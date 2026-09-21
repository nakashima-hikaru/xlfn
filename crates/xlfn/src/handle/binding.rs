//! Formula-binding ownership and non-owning read-side publication.
//!
//! Every live binding slot uniquely owns its record. Removal transfers it to
//! a generation-bound queue while still holding the table writer lock; the
//! record remains owned there until read-domain maintenance or final seal.
//! Atomic publication exposes only a pointer; the call-scoped read domain
//! protects that pointer and the record's object capability while a call reads
//! it.

#![allow(
    unsafe_code,
    reason = "binding reads use audited non-owning pointers protected by the read domain"
)]

#[cfg(test)]
use super::domain::HandleBindingDomainPermit;
use super::domain::HandleReadDomain;
use super::object::{ObjectBinding, ObjectCell};
use super::token::HandleId;
use crate::error::DomainErrorCode;
use crate::generation::BindingGeneration;
use crate::{XllError, XllResult};
use parking_lot::{RwLock, RwLockWriteGuard};
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, AtomicU8, Ordering};
use xlfn_kernel::published_owner::PublishedOwner;

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
    object: ObjectBinding,
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
            object,
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
        self.object.duplicate()
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct BindingPtr(pub(crate) NonNull<BindingRecord>);

impl BindingPtr {
    fn from_ref(record: &BindingRecord) -> Self {
        Self(NonNull::from(record))
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
pub(crate) struct BindingReadLease<'domain> {
    record: BindingPtr,
    _protection: BindingReadProtection<'domain>,
}

enum BindingReadProtection<'domain> {
    Scoped {
        _witness: super::HandleDomainWitness<'domain>,
    },
    #[cfg(test)]
    Standalone {
        _permit: HandleBindingDomainPermit<'domain>,
    },
}

impl<'domain> BindingReadLease<'domain> {
    pub(crate) fn record(&self) -> &BindingRecord {
        // SAFETY: self holds a valid BindingReadLease protected by HandleReadDomain,
        // guaranteeing that the BindingRecord has not been reclaimed.
        unsafe { self.record.0.as_ref() }
    }

    pub(crate) fn object(&self) -> &ObjectCell {
        self.record().object()
    }

    pub(crate) fn duplicate_object_binding(&self) -> XllResult<ObjectBinding> {
        self.record().duplicate_object_binding()
    }

    #[cfg(any(feature = "async", test))]
    pub(crate) fn acquire_object_lease(&self) -> XllResult<super::object::RawObjectLeaseGuard> {
        self.record().object.acquire_lease()
    }
}

mod protocol;

const BINDINGS_PER_PAGE: usize = 256;
type BindingPage = [AtomicPtr<BindingRecord>; BINDINGS_PER_PAGE];

pub(crate) struct PublishedBindings {
    // Pages are initialized once and stay at fixed addresses until registry
    // destruction. Lookup needs no table lock and page lifetime requires no
    // second reclamation protocol alongside the binding read domain.
    pages: Box<[OnceLock<Box<BindingPage>>]>,
}

impl PublishedBindings {
    pub(crate) fn new(maximum_bindings: u32) -> Self {
        Self {
            pages: (0..(maximum_bindings as usize).div_ceil(BINDINGS_PER_PAGE))
                .map(|_| OnceLock::new())
                .collect(),
        }
    }

    pub(crate) fn load(&self, slot: u32) -> BindingSnapshot {
        let record = self
            .entry(slot)
            .and_then(|entry| NonNull::new(entry.load(Ordering::Acquire)))
            .map(BindingPtr);
        BindingSnapshot { record }
    }

    #[inline]
    fn entry(&self, slot: u32) -> Option<&AtomicPtr<BindingRecord>> {
        let slot = slot as usize;
        let page = self.pages.get(slot / BINDINGS_PER_PAGE)?.get()?;
        Some(&page[slot % BINDINGS_PER_PAGE])
    }
}

/// The publication mutator borrows the actual matching write guard for every
/// empty check/store or removal CAS. Its exclusive borrow prevents both guard
/// release and a second writer capability until the mutation finishes.
struct PublicationWriter<'guard, 'table> {
    published: &'table PublishedBindings,
    _guard: &'guard mut RwLockWriteGuard<'table, RegistryState>,
}

impl PublicationWriter<'_, '_> {
    fn insert(&mut self, id: HandleId, record: BindingPtr) {
        let slot = id.slot as usize;
        let Some(page) = self.published.pages.get(slot / BINDINGS_PER_PAGE) else {
            xlfn_kernel::invariant::fail_stop();
        };
        let page = page.get_or_init(|| {
            Box::new([const { AtomicPtr::new(std::ptr::null_mut()) }; BINDINGS_PER_PAGE])
        });
        let entry = &page[slot % BINDINGS_PER_PAGE];
        protocol::publish_binding!(
            entry.load(Ordering::Acquire).is_null(),
            xlfn_kernel::invariant::fail_stop(),
            entry.store(record.0.as_ptr(), Ordering::Release)
        );
    }

    fn remove(&mut self, id: HandleId, expected: BindingPtr) {
        let Some(entry) = self.published.entry(id.slot) else {
            xlfn_kernel::invariant::fail_stop();
        };
        if entry
            .compare_exchange(
                expected.0.as_ptr(),
                std::ptr::null_mut(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            // A removal is serialized with the binding table write lock and
            // must clear the exact pointer it observed. Any mismatch means
            // the publication invariant has already been violated.
            xlfn_kernel::invariant::fail_stop();
        }
    }
}

pub(crate) struct BindingSlot {
    pub(crate) next_generation: BindingGeneration,
    pub(crate) record: Option<PublishedOwner<BindingRecord>>,
}

pub(crate) struct RegistryState {
    pub(crate) slots: Vec<BindingSlot>,
    pub(crate) free: Vec<usize>,
    pub(crate) live_bindings: u32,
}

pub(crate) struct BindingTable {
    state: RwLock<RegistryState>,
    published: PublishedBindings,
    read_domain: PublishedOwner<HandleReadDomain>,
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
            read_domain: PublishedOwner::new(HandleReadDomain::new()),
            maximum_bindings,
        }
    }

    fn publication_writer<'guard, 'table>(
        &'table self,
        guard: &'guard mut RwLockWriteGuard<'table, RegistryState>,
    ) -> Option<PublicationWriter<'guard, 'table>> {
        if !std::ptr::eq(RwLockWriteGuard::rwlock(guard), &self.state) {
            return None;
        }
        Some(PublicationWriter {
            published: &self.published,
            _guard: guard,
        })
    }

    pub(crate) fn read_domain(&self) -> &HandleReadDomain {
        &self.read_domain
    }

    #[inline]
    pub(crate) fn read_scoped<'domain>(
        &self,
        id: HandleId,
        witness: super::HandleDomainWitness<'domain>,
    ) -> XllResult<BindingReadLease<'domain>> {
        protocol::read_binding!(record, record_ref;
            witness.domain() == NonNull::from(self.read_domain()),
            xlfn_kernel::invariant::fail_stop(),
            self.published.load(id.slot).record,
            return Err(XllError::StaleHandle),
            // SAFETY: authorization precedes loading and dereferencing a pointer
            // published by this domain. The retained witness prevents reclamation.
            unsafe { record.0.as_ref() },
            record_ref.id == id && record_ref.state() == BindingState::Live,
            return Err(XllError::StaleHandle),
            Ok(BindingReadLease {
                record,
                _protection: BindingReadProtection::Scoped { _witness: witness },
            })
        )
    }

    #[cfg(test)]
    pub(crate) fn read_standalone(&self, id: HandleId) -> XllResult<BindingReadLease<'_>> {
        let permit = self.read_domain.enter()?;
        let snapshot = self.published.load(id.slot);
        let record = snapshot.record.ok_or(XllError::StaleHandle)?;
        // SAFETY: permit guarantees the binding record cannot be reclaimed while entering.
        let record_ref = unsafe { record.0.as_ref() };
        if record_ref.id != id || record_ref.state() != BindingState::Live {
            return Err(XllError::StaleHandle);
        }
        Ok(BindingReadLease {
            record,
            _protection: BindingReadProtection::Standalone { _permit: permit },
        })
    }

    pub(crate) fn reserve(&self) -> XllResult<BindingReservation<'_>> {
        let mut state = self.state.write();
        // A remover may itself hold a call permit and must not wait for it.
        // Stop new publication at hard debt even in that case. With the live
        // binding cap this bounds total records by maximum_bindings + 256,
        // including batches whose arbitrary destructors are still running.
        if self.read_domain.debt() >= super::domain::HARD_DEBT_LIMIT {
            return Err(XllError::Overloaded);
        }
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
                // Exhausted generations permanently consume their slot. Do
                // not treat padding in the last publication page as capacity.
                if index >= self.maximum_bindings as usize {
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

    pub(crate) fn begin_removal(&self, id: HandleId) -> XllResult<BindingRemoval<'_>> {
        let state = self.state.write();
        let record = state
            .slots
            .get(id.slot as usize)
            .and_then(|slot| slot.record.as_deref())
            .filter(|record| record.id == id)
            .map(BindingPtr::from_ref)
            .ok_or(XllError::StaleHandle)?;
        Ok(BindingRemoval {
            table: self,
            state: Some(state),
            id,
            record,
            active: true,
        })
    }

    pub(crate) fn retire_all(&self) -> (u32, Vec<PublishedOwner<BindingRecord>>) {
        let mut state = self.state.write();
        let live_bindings = state.live_bindings;
        let mut retired = Vec::with_capacity(live_bindings as usize);
        state.free.clear();
        for index in 0..state.slots.len() {
            if let Some(record) = state.slots[index].record.take() {
                // Touch only live publications, independent of page capacity.
                self.publication_writer(&mut state)
                    .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop())
                    .remove(record.id, BindingPtr::from_ref(&record));
                record
                    .state
                    .store(BindingState::Retired as u8, Ordering::Release);
                retired.push(record);
            }
            let slot = &mut state.slots[index];
            if let Some(next) = slot.next_generation.next() {
                slot.next_generation = next;
                state.free.push(index);
            }
        }
        state.live_bindings = 0;
        drop(state);
        self.read_domain.quiesce();
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
        let record = PublishedOwner::new(BindingRecord::new(self.id, object));
        let slot = &mut state.slots[self.index];
        slot.record = Some(record);
        let pointer = BindingPtr::from_ref(slot.record.as_ref().unwrap().as_ref());
        self.table
            .publication_writer(&mut state)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop())
            .insert(self.id, pointer);
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
        // SAFETY: self owns the table write lock and the slot holds the live record.
        unsafe { self.record.0.as_ref() }.object()
    }

    pub(crate) fn commit(mut self) -> bool {
        let mut state = self
            .state
            .take()
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
            .publication_writer(&mut state)
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
            state.free.push(self.id.slot as usize);
        }
        self.active = false;
        // Enqueue before releasing the table lock: seal's retire_all must
        // not pass this removal and drain the domain before it is registered.
        self.table.read_domain.enqueue_reclaim(retired);
        drop(state);
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::registry::HandleRegistry;

    #[test]
    fn miri_publication_writer_requires_matching_table_guard() {
        let first = BindingTable::new(1);
        let second = BindingTable::new(1);
        let mut first_guard = first.state.write();
        {
            let writer = first.publication_writer(&mut first_guard).unwrap();
            assert!(std::ptr::eq(writer.published, &first.published));
            assert!(second.publication_writer(&mut first_guard).is_none());
            assert!(first.state.try_write().is_none());
        }
        drop(first_guard);
        assert!(first.state.try_write().is_some());
        let mut second_guard = second.state.write();
        assert!(first.publication_writer(&mut second_guard).is_none());
        assert!(second.publication_writer(&mut second_guard).is_some());
    }

    #[test]
    fn zero_limit_and_exhausted_slots_do_not_publish_into_page_padding() {
        let empty = HandleRegistry::from_entropy(0, [7; 40]);
        assert!(empty.bindings.published.pages.is_empty());
        assert!(empty.insert_pending(&mut Some(1_usize)).is_err());
        assert!(empty.bindings.published.load(0).record.is_none());

        let registry = HandleRegistry::from_entropy(1, [7; 40]);
        let token = registry.insert_pending(&mut Some(1_usize)).unwrap();
        registry.remove::<usize>(&token).unwrap();
        registry.bindings.write_state().slots[0].next_generation =
            BindingGeneration::new(u64::MAX).unwrap();
        let token = registry.insert_pending(&mut Some(2_usize)).unwrap();
        registry.remove::<usize>(&token).unwrap();
        assert!(matches!(
            registry.insert_pending(&mut Some(3_usize)),
            Err(XllError::Domain {
                code: DomainErrorCode::Overflow
            })
        ));
        assert!(registry.bindings.published.load(1).record.is_none());
    }

    #[test]
    fn sparse_publication_allocates_only_used_pages_and_reuses_them() {
        let registry = HandleRegistry::from_entropy(1_048_576, [7; 40]);
        let published = &registry.bindings.published;
        let allocated_pages = || {
            published
                .pages
                .iter()
                .filter(|page| page.get().is_some())
                .count()
        };
        assert_eq!(allocated_pages(), 0);
        assert!(published.load(1_048_575).record.is_none());
        assert!(published.load(u32::MAX).record.is_none());

        let mut tokens = Vec::new();
        for value in 0..=BINDINGS_PER_PAGE {
            tokens.push(registry.insert_pending(&mut Some(value)).unwrap());
        }
        assert_eq!(allocated_pages(), 2);
        for (value, token) in tokens.iter().enumerate() {
            assert_eq!(registry.lookup::<usize>(token).unwrap(), value);
        }
        let page = published.pages[1].get().unwrap().as_ref().as_ptr();
        registry.remove::<usize>(tokens.last().unwrap()).unwrap();
        assert!(registry.lookup::<usize>(tokens.last().unwrap()).is_err());
        let replacement = registry.insert_pending(&mut Some(99_usize)).unwrap();
        assert_eq!(registry.lookup::<usize>(&replacement).unwrap(), 99);
        assert_eq!(published.pages[1].get().unwrap().as_ref().as_ptr(), page);
        assert_eq!(allocated_pages(), 2);

        registry.retire_values_for_seal();
        for slot in 0..=BINDINGS_PER_PAGE {
            assert!(published.load(slot as u32).record.is_none());
        }
        assert_eq!(
            allocated_pages(),
            2,
            "pages remain stable until registry drop"
        );
    }
}
