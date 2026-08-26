//! Fail-stop primitives for internal ownership accounting.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Stops the process after an internal ownership invariant is violated.
#[cold]
pub fn fail_stop() -> ! {
    std::process::abort()
}

/// Subtracts an internal accounting value without allowing underflow to be
/// converted into a valid-looking state.
#[inline]
pub fn checked_sub_or_abort(value: usize, amount: usize) -> usize {
    value.checked_sub(amount).unwrap_or_else(|| fail_stop())
}

/// Atomically subtracts an internal accounting counter.
///
/// An underflow is an ownership bug, not an application error. The process is
/// therefore stopped instead of wrapping the counter and poisoning a later
/// quiescence decision. The returned value is the value observed before the
/// subtraction.
#[inline]
pub fn checked_atomic_sub(counter: &AtomicUsize, amount: usize) -> usize {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_sub(amount)
        })
        .unwrap_or_else(|_| fail_stop())
}

/// Atomically releases one internal accounting unit.
#[inline]
pub fn checked_atomic_dec(counter: &AtomicUsize) -> usize {
    checked_atomic_sub(counter, 1)
}

/// Atomically subtracts a `u64` internal accounting counter.
///
/// The returned value is the value observed before the subtraction.
#[inline]
pub fn checked_atomic_sub_u64(counter: &AtomicU64, amount: u64) -> u64 {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            value.checked_sub(amount)
        })
        .unwrap_or_else(|_| fail_stop())
}

/// Atomically releases one `u64` internal accounting unit.
#[inline]
pub fn checked_atomic_dec_u64(counter: &AtomicU64) -> u64 {
    checked_atomic_sub_u64(counter, 1)
}
