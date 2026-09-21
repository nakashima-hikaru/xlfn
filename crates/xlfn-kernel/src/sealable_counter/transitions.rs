//! Single Source of Truth (SSOT) state transitions for SealableCounter.
//!
//! Provides pure transition functions used by both production Rust (`xlfn-kernel`)
//! and formal verification (`verification/verus/sealable_counter`).
//!
//! Certified under Verus for both 32-bit (i686) and 64-bit platforms.

#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg(not(verus_only))]
macro_rules! verus {
    ($($item:tt)*) => {
        $($item)*
    };
}

verus! {

// 32-bit Bit Layout Constants (i686-pc-windows-msvc)
pub const SEALED_BIT_32: u32 = 0x8000_0000;
pub const WAITING_BIT_32: u32 = 0x4000_0000;
pub const ACTIVE_COUNT_MASK_32: u32 = 0x3FFF_FFFF;

// 64-bit Bit Layout Constants (x86_64, aarch64)
pub const SEALED_BIT_64: u64 = 0x8000_0000_0000_0000;
pub const WAITING_BIT_64: u64 = 0x4000_0000_0000_0000;
pub const ACTIVE_COUNT_MASK_64: u64 = 0x3FFF_FFFF_FFFF_FFFF;

/// The explicit outcome of a state transition, distinguishing protocol rejection
/// (e.g. counter is sealed or waiter count is retained) from catastrophic invariant violation
/// (e.g. counter overflow or underflow requiring fail_stop).
#[cfg_attr(verus_only, verifier::allow(autoderive_clone_without_spec))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionOutcome<T> {
    Success(T),
    Rejected,
    FailStop,
}

/// The result of releasing one active permit.
#[cfg_attr(verus_only, verifier::allow(autoderive_clone_without_spec))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseOutcome {
    StillActive,
    BecameIdle,
}

}

// ============================================================================
// Shared Pure Transition Logic (Single Source of Truth)
// ============================================================================

macro_rules! acquire_32_logic {
    ($s:ident) => {
        if $s & SEALED_BIT_32 != 0 {
            TransitionOutcome::Rejected
        } else if $s & ACTIVE_COUNT_MASK_32 == ACTIVE_COUNT_MASK_32 {
            TransitionOutcome::FailStop
        } else {
            TransitionOutcome::Success($s.wrapping_add(1))
        }
    };
}

macro_rules! acquire_64_logic {
    ($s:ident) => {
        if $s & SEALED_BIT_64 != 0 {
            TransitionOutcome::Rejected
        } else if $s & ACTIVE_COUNT_MASK_64 == ACTIVE_COUNT_MASK_64 {
            TransitionOutcome::FailStop
        } else {
            TransitionOutcome::Success($s.wrapping_add(1))
        }
    };
}

macro_rules! release_32_logic {
    ($s:ident) => {
        if $s & ACTIVE_COUNT_MASK_32 == 0 {
            TransitionOutcome::FailStop
        } else {
            TransitionOutcome::Success($s.wrapping_sub(1))
        }
    };
}

macro_rules! release_64_logic {
    ($s:ident) => {
        if $s & ACTIVE_COUNT_MASK_64 == 0 {
            TransitionOutcome::FailStop
        } else {
            TransitionOutcome::Success($s.wrapping_sub(1))
        }
    };
}

macro_rules! release_without_notification_32_logic {
    ($s:ident) => {{
        let active = $s & ACTIVE_COUNT_MASK_32;
        if active == 0 {
            TransitionOutcome::FailStop
        } else if active == 1 && ($s & WAITING_BIT_32 != 0) {
            TransitionOutcome::Rejected
        } else {
            TransitionOutcome::Success($s.wrapping_sub(1))
        }
    }};
}

macro_rules! release_without_notification_64_logic {
    ($s:ident) => {{
        let active = $s & ACTIVE_COUNT_MASK_64;
        if active == 0 {
            TransitionOutcome::FailStop
        } else if active == 1 && ($s & WAITING_BIT_64 != 0) {
            TransitionOutcome::Rejected
        } else {
            TransitionOutcome::Success($s.wrapping_sub(1))
        }
    }};
}

macro_rules! release_outcome_32_logic {
    ($p:ident) => {
        if $p & ACTIVE_COUNT_MASK_32 == 1 {
            ReleaseOutcome::BecameIdle
        } else {
            ReleaseOutcome::StillActive
        }
    };
}

macro_rules! release_outcome_64_logic {
    ($p:ident) => {
        if $p & ACTIVE_COUNT_MASK_64 == 1 {
            ReleaseOutcome::BecameIdle
        } else {
            ReleaseOutcome::StillActive
        }
    };
}

macro_rules! reopen_32_logic {
    ($s:ident) => {
        if $s & SEALED_BIT_32 != 0 && $s & ACTIVE_COUNT_MASK_32 == 0 {
            TransitionOutcome::Success(0)
        } else {
            TransitionOutcome::Rejected
        }
    };
}

macro_rules! reopen_64_logic {
    ($s:ident) => {
        if $s & SEALED_BIT_64 != 0 && $s & ACTIVE_COUNT_MASK_64 == 0 {
            TransitionOutcome::Success(0)
        } else {
            TransitionOutcome::Rejected
        }
    };
}

// ============================================================================
// Standard Rust Implementations (for cargo build)
// ============================================================================

#[cfg(not(verus_only))]
#[inline]
pub fn acquire_step_32(state: u32) -> TransitionOutcome<u32> {
    acquire_32_logic!(state)
}

#[cfg(not(verus_only))]
#[inline]
pub fn acquire_step_64(state: u64) -> TransitionOutcome<u64> {
    acquire_64_logic!(state)
}

#[cfg(not(verus_only))]
#[inline]
pub fn release_step_32(state: u32) -> TransitionOutcome<u32> {
    release_32_logic!(state)
}

#[cfg(not(verus_only))]
#[inline]
pub fn release_step_64(state: u64) -> TransitionOutcome<u64> {
    release_64_logic!(state)
}

#[cfg(not(verus_only))]
#[inline]
pub fn release_without_notification_step_32(state: u32) -> TransitionOutcome<u32> {
    release_without_notification_32_logic!(state)
}

#[cfg(not(verus_only))]
#[inline]
pub fn release_without_notification_step_64(state: u64) -> TransitionOutcome<u64> {
    release_without_notification_64_logic!(state)
}

#[cfg(not(verus_only))]
#[inline]
pub fn release_outcome_step_32(previous: u32) -> ReleaseOutcome {
    release_outcome_32_logic!(previous)
}

#[cfg(not(verus_only))]
#[inline]
pub fn release_outcome_step_64(previous: u64) -> ReleaseOutcome {
    release_outcome_64_logic!(previous)
}

#[cfg(not(verus_only))]
#[inline]
pub fn reopen_step_32(state: u32) -> TransitionOutcome<u32> {
    reopen_32_logic!(state)
}

#[cfg(not(verus_only))]
#[inline]
pub fn reopen_step_64(state: u64) -> TransitionOutcome<u64> {
    reopen_64_logic!(state)
}

// ============================================================================
// Verus Verified Specifications & Executable Functions (for verus)
// ============================================================================

#[cfg(verus_only)]
verus! {

pub open spec fn acquire_spec_32(state: u32) -> TransitionOutcome<u32> {
    acquire_32_logic!(state)
}

pub open spec fn acquire_spec_64(state: u64) -> TransitionOutcome<u64> {
    acquire_64_logic!(state)
}

pub open spec fn release_spec_32(state: u32) -> TransitionOutcome<u32> {
    release_32_logic!(state)
}

pub open spec fn release_spec_64(state: u64) -> TransitionOutcome<u64> {
    release_64_logic!(state)
}

pub open spec fn release_without_notification_spec_32(state: u32) -> TransitionOutcome<u32> {
    release_without_notification_32_logic!(state)
}

pub open spec fn release_without_notification_spec_64(state: u64) -> TransitionOutcome<u64> {
    release_without_notification_64_logic!(state)
}

pub open spec fn release_outcome_spec_32(previous: u32) -> ReleaseOutcome {
    release_outcome_32_logic!(previous)
}

pub open spec fn release_outcome_spec_64(previous: u64) -> ReleaseOutcome {
    release_outcome_64_logic!(previous)
}

pub open spec fn reopen_spec_32(state: u32) -> TransitionOutcome<u32> {
    reopen_32_logic!(state)
}

pub open spec fn reopen_spec_64(state: u64) -> TransitionOutcome<u64> {
    reopen_64_logic!(state)
}

#[inline]
pub fn acquire_step_32(state: u32) -> (res: TransitionOutcome<u32>)
    ensures res == acquire_spec_32(state)
{
    acquire_32_logic!(state)
}

#[inline]
pub fn acquire_step_64(state: u64) -> (res: TransitionOutcome<u64>)
    ensures res == acquire_spec_64(state)
{
    acquire_64_logic!(state)
}

#[inline]
pub fn release_step_32(state: u32) -> (res: TransitionOutcome<u32>)
    ensures res == release_spec_32(state)
{
    release_32_logic!(state)
}

#[inline]
pub fn release_step_64(state: u64) -> (res: TransitionOutcome<u64>)
    ensures res == release_spec_64(state)
{
    release_64_logic!(state)
}

#[inline]
pub fn release_without_notification_step_32(state: u32) -> (res: TransitionOutcome<u32>)
    ensures res == release_without_notification_spec_32(state)
{
    release_without_notification_32_logic!(state)
}

#[inline]
pub fn release_without_notification_step_64(state: u64) -> (res: TransitionOutcome<u64>)
    ensures res == release_without_notification_spec_64(state)
{
    release_without_notification_64_logic!(state)
}

#[inline]
pub fn release_outcome_step_32(previous: u32) -> (res: ReleaseOutcome)
    ensures res == release_outcome_spec_32(previous)
{
    release_outcome_32_logic!(previous)
}

#[inline]
pub fn release_outcome_step_64(previous: u64) -> (res: ReleaseOutcome)
    ensures res == release_outcome_spec_64(previous)
{
    release_outcome_64_logic!(previous)
}

#[inline]
pub fn reopen_step_32(state: u32) -> (res: TransitionOutcome<u32>)
    ensures res == reopen_spec_32(state)
{
    reopen_32_logic!(state)
}

#[inline]
pub fn reopen_step_64(state: u64) -> (res: TransitionOutcome<u64>)
    ensures res == reopen_spec_64(state)
{
    reopen_64_logic!(state)
}

}

// ============================================================================
// Platform-Native usize Adapters (for production xlfn-kernel)
// ============================================================================

#[inline]
pub fn acquire_step(state: usize) -> TransitionOutcome<usize> {
    #[cfg(target_pointer_width = "32")]
    {
        match acquire_step_32(state as u32) {
            TransitionOutcome::Success(next) => TransitionOutcome::Success(next as usize),
            TransitionOutcome::Rejected => TransitionOutcome::Rejected,
            TransitionOutcome::FailStop => TransitionOutcome::FailStop,
        }
    }
    #[cfg(target_pointer_width = "64")]
    {
        match acquire_step_64(state as u64) {
            TransitionOutcome::Success(next) => TransitionOutcome::Success(next as usize),
            TransitionOutcome::Rejected => TransitionOutcome::Rejected,
            TransitionOutcome::FailStop => TransitionOutcome::FailStop,
        }
    }
}

#[inline]
pub fn release_step(state: usize) -> TransitionOutcome<usize> {
    #[cfg(target_pointer_width = "32")]
    {
        match release_step_32(state as u32) {
            TransitionOutcome::Success(next) => TransitionOutcome::Success(next as usize),
            TransitionOutcome::Rejected => TransitionOutcome::Rejected,
            TransitionOutcome::FailStop => TransitionOutcome::FailStop,
        }
    }
    #[cfg(target_pointer_width = "64")]
    {
        match release_step_64(state as u64) {
            TransitionOutcome::Success(next) => TransitionOutcome::Success(next as usize),
            TransitionOutcome::Rejected => TransitionOutcome::Rejected,
            TransitionOutcome::FailStop => TransitionOutcome::FailStop,
        }
    }
}

#[inline]
pub fn release_without_notification_step(state: usize) -> TransitionOutcome<usize> {
    #[cfg(target_pointer_width = "32")]
    {
        match release_without_notification_step_32(state as u32) {
            TransitionOutcome::Success(next) => TransitionOutcome::Success(next as usize),
            TransitionOutcome::Rejected => TransitionOutcome::Rejected,
            TransitionOutcome::FailStop => TransitionOutcome::FailStop,
        }
    }
    #[cfg(target_pointer_width = "64")]
    {
        match release_without_notification_step_64(state as u64) {
            TransitionOutcome::Success(next) => TransitionOutcome::Success(next as usize),
            TransitionOutcome::Rejected => TransitionOutcome::Rejected,
            TransitionOutcome::FailStop => TransitionOutcome::FailStop,
        }
    }
}

#[inline]
pub fn release_outcome_step(previous: usize) -> ReleaseOutcome {
    #[cfg(target_pointer_width = "32")]
    {
        release_outcome_step_32(previous as u32)
    }
    #[cfg(target_pointer_width = "64")]
    {
        release_outcome_step_64(previous as u64)
    }
}

#[inline]
pub fn reopen_step(state: usize) -> TransitionOutcome<usize> {
    #[cfg(target_pointer_width = "32")]
    {
        match reopen_step_32(state as u32) {
            TransitionOutcome::Success(next) => TransitionOutcome::Success(next as usize),
            TransitionOutcome::Rejected => TransitionOutcome::Rejected,
            TransitionOutcome::FailStop => TransitionOutcome::FailStop,
        }
    }
    #[cfg(target_pointer_width = "64")]
    {
        match reopen_step_64(state as u64) {
            TransitionOutcome::Success(next) => TransitionOutcome::Success(next as usize),
            TransitionOutcome::Rejected => TransitionOutcome::Rejected,
            TransitionOutcome::FailStop => TransitionOutcome::FailStop,
        }
    }
}

// The RMW operation and ordering are shared, not just the observation mask.
// `Ordering` resolves to the native/Loom enum or the executable Verus backend.
#[cfg_attr(verus_only, allow(unused_macros))]
macro_rules! mark_waiting {
    ($state:expr, $waiting:expr, $mask:expr) => {
        $state.fetch_or($waiting, Ordering::AcqRel) & $mask
    };
}

pub(crate) use mark_waiting;

#[cfg_attr(verus_only, allow(unused_macros))]
macro_rules! idle_seal_logic {
    ($state:ident, $sealed:expr, $mask:expr) => {
        if $state & ($sealed | $mask) == 0 {
            Some($state | $sealed)
        } else {
            None
        }
    };
}
#[cfg_attr(verus_only, allow(unused_macros))]
macro_rules! undo_idle_seal_logic {
    ($state:ident, $sealed:expr, $mask:expr) => {
        if $state & $sealed != 0 && $state & $mask == 0 {
            Some($state & !$sealed)
        } else {
            None
        }
    };
}
#[cfg_attr(verus_only, allow(unused_macros))]
macro_rules! try_idle_seal {
    ($atomic:expr, $sealed:expr, $mask:expr) => {{
        let state = $atomic.load(Ordering::Acquire);
        match idle_seal_logic!(state, $sealed, $mask) {
            Some(next) => $atomic
                .compare_exchange(state, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            None => false,
        }
    }};
}
pub(crate) use idle_seal_logic;
pub(crate) use try_idle_seal;
pub(crate) use undo_idle_seal_logic;
