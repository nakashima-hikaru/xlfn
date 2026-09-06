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

#[derive(Clone, Copy)]
pub(crate) struct HandleDomainWitness<'scope> {
    domain: NonNull<HandleReadDomain>,
    _marker: PhantomData<&'scope ()>,
}

impl<'scope> HandleDomainWitness<'scope> {
    #[inline]
    pub(crate) fn new(domain: NonNull<HandleReadDomain>) -> Self {
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

    #[inline]
    pub(crate) fn enter_owned(&self) -> XllResult<HandleDomainPermit> {
        // SAFETY: every returned permit is stored in a CallScope that is
        // nested within the generation owner containing this domain. The
        // generation close path drains the domain before reclaiming it.
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
