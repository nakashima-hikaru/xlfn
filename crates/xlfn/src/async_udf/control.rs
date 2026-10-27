//! Synchronized registry state and the unique authority for each close attempt.

use super::calculation::{CalculationEpoch, CalculationState};
use crate::{XllError, XllResult};
use rustc_hash::FxHashMap;
use std::num::NonZeroU64;
use xlfn_kernel::published_owner::PublishedOwner;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ControlPhase {
    Stopped,
    Running,
    Advancing {
        from: CalculationEpoch,
        to: CalculationEpoch,
    },
    Closing,
}

/// Affine identity: only one live claim can advance a close to Stopped.
pub(super) struct CloseAttempt(NonZeroU64);

pub(super) struct RegistryControl {
    phase: ControlPhase,
    close_attempt: Option<NonZeroU64>,
    next_close_attempt: NonZeroU64,
    /// Unique owners remain stable while calculation pins retain their pointers.
    pub(super) calculations: FxHashMap<CalculationEpoch, PublishedOwner<CalculationState>>,
}

impl RegistryControl {
    pub(super) fn new(initial: PublishedOwner<CalculationState>) -> Self {
        Self {
            phase: ControlPhase::Stopped,
            close_attempt: None,
            next_close_attempt: NonZeroU64::MIN,
            calculations: FxHashMap::from_iter([(initial.epoch, initial)]),
        }
    }

    pub(super) fn phase(&self) -> ControlPhase {
        self.phase
    }

    pub(super) fn start(&mut self) {
        if self.phase != ControlPhase::Stopped || self.close_attempt.is_some() {
            xlfn_kernel::invariant::fail_stop();
        }
        self.phase = ControlPhase::Running;
    }

    pub(super) fn begin_advance(&mut self, from: CalculationEpoch, to: CalculationEpoch) {
        if self.phase != ControlPhase::Running {
            xlfn_kernel::invariant::fail_stop();
        }
        self.phase = ControlPhase::Advancing { from, to };
    }

    pub(super) fn finish_advance(&mut self) {
        if !matches!(self.phase, ControlPhase::Advancing { .. }) {
            xlfn_kernel::invariant::fail_stop();
        }
        self.phase = ControlPhase::Running;
    }

    pub(super) fn begin_close(&mut self) -> XllResult<CloseAttempt> {
        if self.close_attempt.is_some() {
            return Err(XllError::Closing);
        }
        let attempt = self.next_close_attempt;
        self.next_close_attempt = attempt
            .checked_add(1)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        self.close_attempt = Some(attempt);
        self.phase = ControlPhase::Closing;
        Ok(CloseAttempt(attempt))
    }

    pub(super) fn abandon_close(&mut self, attempt: CloseAttempt) {
        if self.close_attempt != Some(attempt.0) || self.phase != ControlPhase::Closing {
            xlfn_kernel::invariant::fail_stop();
        }
        // Retry may claim the same Closing state; restart requires a completed close.
        self.close_attempt = None;
    }

    pub(super) fn finish_close(&mut self, attempt: CloseAttempt) {
        if self.close_attempt != Some(attempt.0) || self.phase != ControlPhase::Closing {
            xlfn_kernel::invariant::fail_stop();
        }
        self.close_attempt = None;
        self.phase = ControlPhase::Stopped;
    }
}
