//! Call-scoped handle read domain and deferred reclamation.
//!
//! Rather than acquiring a per-record reader permit on every lookup, UDF
//! calls register once with this domain for the duration of their [`CallScope`].
//! Readers enter striped drain gates partitioned by generation, avoiding
//! cache-line contention on individual binding records during concurrent
//! lookups of the same handle.
//!
//! When bindings are removed or retired, reclamation seals the active
//! generation, reopens the other generation, and then drains the sealed one.
//! Only the current generation is open, so a reader that observed the old
//! generation before rotation cannot be admitted after the grace period starts.

#![allow(
    unsafe_code,
    reason = "owned read permits are audited temporal capabilities"
)]

use crate::{XllError, XllResult};
use xlfn_kernel::drain_gate::DEFAULT_STRIPE_COUNT;
use xlfn_kernel::rotating_read_domain::{
    RotatingReadDomain, RotatingReadOwnedPermit, RotatingReadPermit,
};

use std::marker::PhantomData;
use std::ptr::NonNull;

pub(crate) struct HandleReadDomain {
    domain: RotatingReadDomain<DEFAULT_STRIPE_COUNT>,
}

pub(crate) struct HandleDomainPermit {
    pub(crate) domain: NonNull<HandleReadDomain>,
    _permit: RotatingReadOwnedPermit<DEFAULT_STRIPE_COUNT>,
}

impl HandleDomainPermit {
    #[allow(
        dead_code,
        reason = "Audited capability constructor for tests and scoped readers"
    )]
    #[inline]
    pub(crate) fn witness<'scope>(&'scope self) -> HandleDomainWitness<'scope> {
        HandleDomainWitness {
            domain: self.domain,
            _marker: PhantomData,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct HandleDomainWitness<'scope> {
    domain: NonNull<HandleReadDomain>,
    _marker: PhantomData<&'scope ()>,
}

impl<'scope> HandleDomainWitness<'scope> {
    /// Constructs a witness representing an active read permit for `domain` throughout `'scope`.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that an active reader permit for `domain`
    /// remains valid and will not be reclaimed for the duration of `'scope`.
    #[inline]
    pub(crate) unsafe fn new_unchecked(domain: NonNull<HandleReadDomain>) -> Self {
        Self {
            domain,
            _marker: PhantomData,
        }
    }

    #[inline]
    pub(crate) fn domain(&self) -> NonNull<HandleReadDomain> {
        self.domain
    }
}

pub(crate) type HandleBindingDomainPermit<'domain> =
    RotatingReadPermit<'domain, DEFAULT_STRIPE_COUNT>;

impl HandleReadDomain {
    pub(crate) fn new() -> Self {
        Self {
            domain: RotatingReadDomain::new(),
        }
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn enter(&self) -> XllResult<HandleBindingDomainPermit<'_>> {
        self.domain
            .enter_current_thread()
            .map_err(|_| XllError::Closing)
    }

    /// Enters the handle read domain and acquires an owned reader permit.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `self` outlives the returned [`HandleDomainPermit`].
    /// All permits must be dropped before the domain is destroyed or moved.
    #[inline]
    pub(crate) unsafe fn enter_owned(&self) -> XllResult<HandleDomainPermit> {
        // SAFETY: guaranteed by caller's owner-lifetime contract;
        // self outlives the returned permit and will be drained before reclamation.
        unsafe {
            self.domain
                .enter_owned_current_thread()
                .map(|permit| HandleDomainPermit {
                    domain: NonNull::from(self),
                    _permit: permit,
                })
                .map_err(|_| XllError::Closing)
        }
    }

    /// Rotates the read generation and waits for all readers admitted to the
    /// sealed generation to complete.
    pub(crate) fn quiesce(&self) {
        let _ = self.domain.quiesce(|_| {});
    }

    /// Seals both generations of the domain, preventing new readers and
    /// draining existing ones.
    pub(crate) fn seal(&self) {
        self.domain.seal_and_wait();
    }
}
