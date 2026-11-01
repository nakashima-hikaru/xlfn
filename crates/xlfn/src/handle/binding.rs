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
#[cfg(test)]
use crate::sync::RwLockReadGuard;
use crate::sync::{RwLock, RwLockWriteGuard};
use crate::{XllError, XllResult};
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
    object: ObjectBinding,
    state: AtomicU8,
}

// SAFETY: BindingRecord is uniquely owned by BindingTable and transferred across threads.
unsafe impl Send for BindingRecord {}
// SAFETY: BindingRecord is immutable during publication and thread-safe.
unsafe impl Sync for BindingRecord {}

impl BindingRecord {
    fn new(id: HandleId, object: ObjectBinding) -> Self {
        Self {
            id,
            object,
            state: AtomicU8::new(BindingState::Live as u8),
        }
    }

    pub(crate) fn state(&self) -> BindingState {
        BindingState::from_raw(self.state.load(Ordering::Acquire))
    }

    #[inline(always)]
    pub(crate) fn object(&self) -> &ObjectCell {
        self.object.object()
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
mod transaction;

use transaction::PublishedBindingOwner;
pub(crate) use transaction::{BindingRemoval, BindingReservation};

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

/// An exclusive writer is issued by one table and retains that exact table's
/// publication storage with its guard. Callers never re-pair a guard and table.
struct BindingWriter<'table> {
    state: RwLockWriteGuard<'table, RegistryState>,
    published: &'table PublishedBindings,
    domain: &'table HandleReadDomain,
}
impl std::ops::Deref for BindingWriter<'_> {
    type Target = RegistryState;
    fn deref(&self) -> &RegistryState {
        &self.state
    }
}
impl std::ops::DerefMut for BindingWriter<'_> {
    fn deref_mut(&mut self) -> &mut RegistryState {
        &mut self.state
    }
}
impl BindingWriter<'_> {
    fn publish_slot(&mut self, index: usize) {
        let record = self.state.slots[index]
            .record
            .as_ref()
            .expect("the reservation installed its owner");
        let id = record.id;
        let pointer = BindingPtr::from_ref(record.as_ref());
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
            entry.store(pointer.0.as_ptr(), Ordering::Release)
        );
    }

    fn withdraw_slot(&mut self, index: usize) -> PublishedBindingOwner {
        let record = self.state.slots[index]
            .record
            .take()
            .expect("the removal owns the validated slot");
        record
            .state
            .store(BindingState::Retired as u8, Ordering::Release);
        let Some(entry) = self.published.entry(record.id.slot) else {
            xlfn_kernel::invariant::fail_stop();
        };
        // This validation concerns the shared atomic publication itself. The
        // matching guard, slot owner and expected pointer are derived here.
        if entry
            .compare_exchange(
                std::ptr::from_ref(record.as_ref()).cast_mut(),
                std::ptr::null_mut(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            xlfn_kernel::invariant::fail_stop();
        }
        record
    }
}

pub(crate) struct BindingSlot {
    pub(crate) next_generation: BindingGeneration,
    pub(crate) record: Option<PublishedBindingOwner>,
}

pub(crate) struct RegistryState {
    pub(crate) slots: Vec<BindingSlot>,
    pub(crate) free: Vec<u32>,
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

    fn writer(&self) -> BindingWriter<'_> {
        BindingWriter {
            state: self.state.write(),
            published: &self.published,
            domain: &self.read_domain,
        }
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
        BindingReservation::acquire(self)
    }

    #[cfg(test)]
    pub(crate) fn read_state(&self) -> RwLockReadGuard<'_, RegistryState> {
        self.state.read()
    }

    #[cfg(test)]
    pub(crate) fn try_read_state(&self) -> Option<RwLockReadGuard<'_, RegistryState>> {
        self.state.try_read()
    }

    #[cfg(test)]
    pub(crate) fn write_state(&self) -> RwLockWriteGuard<'_, RegistryState> {
        self.state.write()
    }

    pub(crate) fn begin_removal(&self, id: HandleId) -> XllResult<BindingRemoval<'_>> {
        BindingRemoval::acquire(self, id)
    }

    pub(crate) fn retire_all(&self) -> (u32, transaction::BulkBindingsDrained) {
        transaction::retire_all(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handle::registry::HandleRegistry;

    #[test]
    fn miri_binding_writer_keeps_its_issuing_table_and_guard() {
        let first = BindingTable::new(1);
        let second = BindingTable::new(1);
        let writer = first.writer();
        assert!(std::ptr::eq(writer.published, &first.published));
        assert!(!std::ptr::eq(writer.published, &second.published));
        assert!(first.state.try_write().is_none());
        assert!(second.state.try_write().is_some());
        drop(writer);
        assert!(first.state.try_write().is_some());
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
