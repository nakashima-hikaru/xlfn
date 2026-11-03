use super::*;
use std::marker::PhantomData;

/// Management state not yet classified under the unique control claim.
#[derive(Debug)]
pub enum GateUnknown {}
/// Admission has been sealed under this control claim.
#[derive(Debug)]
pub enum GateSealed {}
/// Admission is sealed and its final notification tail has completed.
#[derive(Debug)]
pub enum GateDrained {}
/// Admission has been reopened by this control claim.
#[derive(Debug)]
pub enum GateOpen {}

/// Exclusive reusable-gate management. The claim mutex is distinct from the
/// final-release notification mutex; waiting never holds the latter while
/// releases are trying to acquire it. Dropping/forgetting a control cannot
/// reopen admission or reclaim any owner.
#[must_use = "retain exclusive management until the transition is complete"]
pub struct GateControl<'a, Phase> {
    gate: &'a DrainGate,
    claim: MutexGuard<'a, ()>,
    phase: PhantomData<Phase>,
}

impl<'a> GateControl<'a, GateUnknown> {
    pub(super) fn claim(gate: &'a DrainGate) -> Self {
        Self {
            gate,
            claim: gate.control.lock(),
            phase: PhantomData,
        }
    }
    pub(super) fn try_claim(gate: &'a DrainGate) -> Option<Self> {
        Some(Self {
            gate,
            claim: gate.control.try_lock()?,
            phase: PhantomData,
        })
    }
}

impl<'a, Phase> GateControl<'a, Phase> {
    pub fn seal(self) -> GateControl<'a, GateSealed> {
        self.gate.seal();
        GateControl {
            gate: self.gate,
            claim: self.claim,
            phase: PhantomData,
        }
    }
}

impl<'a> GateControl<'a, GateUnknown> {
    /// Open-gate idle observation only; this does not produce a drained
    /// capability and cannot authorize reopening or allocation reclamation.
    pub fn wait_idle(self) -> Self {
        self.gate.wait_until_idle();
        self
    }

    /// Recognizes sealed admission, retaining the claim while waiting for the
    /// final notification tail. Unlike try_drained this may wait for releases.
    pub fn wait_if_sealed(self) -> Result<GateControl<'a, GateDrained>, Self> {
        if !self.gate.is_sealed() {
            return Err(self);
        }
        self.gate.wait_until_idle();
        Ok(GateControl {
            gate: self.gate,
            claim: self.claim,
            phase: PhantomData,
        })
    }

    pub fn try_drained(self) -> Result<GateControl<'a, GateDrained>, Self> {
        if !self.gate.is_sealed() {
            return Err(self);
        }
        let Some(_notification) = self.gate.idle.lock.try_lock() else {
            return Err(self);
        };
        if self.gate.counter.mark_waiting() != 0 {
            return Err(self);
        }
        Ok(GateControl {
            gate: self.gate,
            claim: self.claim,
            phase: PhantomData,
        })
    }
}

impl<'a> GateControl<'a, GateSealed> {
    pub fn wait(self) -> GateControl<'a, GateDrained> {
        self.gate.wait_until_idle();
        GateControl {
            gate: self.gate,
            claim: self.claim,
            phase: PhantomData,
        }
    }
}

impl<'a> GateControl<'a, GateDrained> {
    pub fn reopen(self) -> GateControl<'a, GateOpen> {
        self.gate
            .reopen()
            .unwrap_or_else(|_| crate::invariant::fail_stop());
        GateControl {
            gate: self.gate,
            claim: self.claim,
            phase: PhantomData,
        }
    }
}

#[must_use = "retain exclusive management until the transition is complete"]
pub struct StripedGateControl<'a, Phase, const N: usize> {
    gate: &'a StripedDrainGate<N>,
    claim: MutexGuard<'a, ()>,
    phase: PhantomData<Phase>,
}

impl<'a, const N: usize> StripedGateControl<'a, GateUnknown, N> {
    pub(super) fn claim(gate: &'a StripedDrainGate<N>) -> Self {
        Self {
            gate,
            claim: gate.control.lock(),
            phase: PhantomData,
        }
    }
    pub(super) fn try_claim(gate: &'a StripedDrainGate<N>) -> Option<Self> {
        Some(Self {
            gate,
            claim: gate.control.try_lock()?,
            phase: PhantomData,
        })
    }
}

impl<'a, Phase, const N: usize> StripedGateControl<'a, Phase, N> {
    pub fn seal(self) -> StripedGateControl<'a, GateSealed, N> {
        self.gate.seal();
        StripedGateControl {
            gate: self.gate,
            claim: self.claim,
            phase: PhantomData,
        }
    }
}

impl<'a, const N: usize> StripedGateControl<'a, GateUnknown, N> {
    /// Idle snapshot while admission can remain open. No stable drain proof
    /// is created by this observation.
    pub fn wait_idle(self) -> Self {
        self.gate.wait_until_idle();
        self
    }

    pub fn wait_if_sealed(self) -> Result<StripedGateControl<'a, GateDrained, N>, Self> {
        if !self.gate.is_sealed() {
            return Err(self);
        }
        self.gate.wait_until_idle();
        Ok(StripedGateControl {
            gate: self.gate,
            claim: self.claim,
            phase: PhantomData,
        })
    }

    pub fn try_drained(self) -> Result<StripedGateControl<'a, GateDrained, N>, Self> {
        if !self.gate.is_sealed() || !self.gate.try_wait_until_idle() {
            return Err(self);
        }
        Ok(StripedGateControl {
            gate: self.gate,
            claim: self.claim,
            phase: PhantomData,
        })
    }

    /// No blocking control or notification mutex is acquired on this path.
    pub fn try_seal_if_idle(self) -> Result<StripedGateControl<'a, GateSealed, N>, Self> {
        if !self.gate.try_seal_if_idle() {
            return Err(self);
        }
        Ok(StripedGateControl {
            gate: self.gate,
            claim: self.claim,
            phase: PhantomData,
        })
    }
}

impl<'a, const N: usize> StripedGateControl<'a, GateSealed, N> {
    pub fn wait(self) -> StripedGateControl<'a, GateDrained, N> {
        self.gate.wait_until_idle();
        StripedGateControl {
            gate: self.gate,
            claim: self.claim,
            phase: PhantomData,
        }
    }

    pub fn try_wait(self) -> Result<StripedGateControl<'a, GateDrained, N>, Self> {
        if !self.gate.try_wait_until_idle() {
            return Err(self);
        }
        Ok(StripedGateControl {
            gate: self.gate,
            claim: self.claim,
            phase: PhantomData,
        })
    }
}

impl<'a, const N: usize> StripedGateControl<'a, GateDrained, N> {
    pub fn reopen(self) -> StripedGateControl<'a, GateOpen, N> {
        self.gate
            .reopen()
            .unwrap_or_else(|_| crate::invariant::fail_stop());
        StripedGateControl {
            gate: self.gate,
            claim: self.claim,
            phase: PhantomData,
        }
    }
}

/// Lifetime-erased permit for an explicitly owner-managed gate. It can only be
/// constructed through an unsafe lifetime contract; there is no safe borrowed
/// permit conversion which erases its lifetime.
#[derive(Debug)]
pub struct DetachedDrainPermit {
    gate: NonNull<DrainGate>,
}

impl DetachedDrainPermit {
    #[inline]
    pub(super) unsafe fn acquire(gate: &DrainGate) -> Result<Self, Sealed> {
        gate.counter.try_acquire()?;
        Ok(Self {
            gate: NonNull::from(gate),
        })
    }
    #[inline]
    pub fn release(self) {
        drop(self);
    }
}

impl Drop for DetachedDrainPermit {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: acquisition's owner-lifetime contract retains this gate
        // through the complete release/notification tail.
        unsafe { DrainGate::release_owned(self.gate) };
    }
}

// SAFETY: the gate is synchronized; its owner-lifetime contract holds across threads.
unsafe impl Send for DetachedDrainPermit {}
// SAFETY: shared references never release or mutate the permit capability.
unsafe impl Sync for DetachedDrainPermit {}

#[derive(Debug)]
pub struct DetachedStripedDrainPermit<const N: usize> {
    gate: NonNull<StripedDrainGate<N>>,
    stripe: usize,
}

impl<const N: usize> DetachedStripedDrainPermit<N> {
    #[inline]
    pub(super) unsafe fn acquire(
        gate: &StripedDrainGate<N>,
        stripe: usize,
    ) -> Result<Self, Sealed> {
        gate.counter(stripe).try_acquire()?;
        Ok(Self {
            gate: NonNull::from(gate),
            stripe,
        })
    }
    #[inline]
    pub fn release(self) {
        drop(self);
    }
}

impl<const N: usize> Drop for DetachedStripedDrainPermit<N> {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: acquisition binds the count to this exact live gate/stripe,
        // retained by its owner through the final notification tail.
        unsafe { StripedDrainGate::release_owned(self.gate, self.stripe) };
    }
}

// SAFETY: release is synchronized and acquisition's lifetime contract is thread-independent.
unsafe impl<const N: usize> Send for DetachedStripedDrainPermit<N> {}
// SAFETY: shared references cannot consume the permit or release its count.
unsafe impl<const N: usize> Sync for DetachedStripedDrainPermit<N> {}
