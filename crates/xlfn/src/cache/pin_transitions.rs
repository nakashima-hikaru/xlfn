//! Cache pin arithmetic and lookup sequencing shared by production and Verus.
//! Zero pins is terminal; overflow is an error, while release underflow aborts.

#[cfg(verus_only)]
use vstd::prelude::*;
#[cfg(not(verus_only))]
macro_rules! verus { ($($item:tt)*) => { $($item)* }; }

verus! {
#[cfg_attr(verus_only, verifier::allow(autoderive_clone_without_spec))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Acquire<T> {
    Acquired(T),
    Zero,
    Overflow,
}

#[cfg_attr(verus_only, verifier::allow(autoderive_clone_without_spec))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Release {
    StillPinned,
    LastPin,
    FailStop,
}
}

macro_rules! acquire_logic {
    ($pins:ident, $word:ty) => {
        if $pins == 0 {
            Acquire::Zero
        } else if $pins == <$word>::MAX {
            Acquire::Overflow
        } else {
            Acquire::Acquired($pins.wrapping_add(1))
        }
    };
}

macro_rules! release_logic {
    ($previous:ident) => {
        if $previous == 0 {
            Release::FailStop
        } else if $previous == 1 {
            Release::LastPin
        } else {
            Release::StillPinned
        }
    };
}

macro_rules! width {
    ($module:ident, $word:ty) => {
        pub(crate) mod $module {
            use super::*;

            #[cfg(not(verus_only))]
            #[inline]
            pub(crate) fn acquire(pins: $word) -> Acquire<$word> {
                acquire_logic!(pins, $word)
            }

            #[cfg(not(verus_only))]
            #[inline]
            pub(crate) fn release(previous: $word) -> Release {
                release_logic!(previous)
            }

            #[cfg(verus_only)]
            verus! {
                pub(crate) open spec fn acquire_spec(pins: $word) -> Acquire<$word> {
                    acquire_logic!(pins, $word)
                }
                pub(crate) open spec fn release_spec(previous: $word) -> Release {
                    release_logic!(previous)
                }
                pub(crate) fn acquire(pins: $word) -> (result: Acquire<$word>)
                    ensures result == acquire_spec(pins),
                { acquire_logic!(pins, $word) }
                pub(crate) fn release(previous: $word) -> (result: Release)
                    ensures result == release_spec(previous),
                { release_logic!(previous) }

                pub(crate) proof fn successful_acquire_adds_one(pins: $word, next: $word)
                    requires acquire_spec(pins) == Acquire::Acquired(next),
                    ensures pins > 0, pins < <$word>::MAX,
                        next == pins + 1, next > 1,
                {
                    assert((pins < <$word>::MAX && next == pins.wrapping_add(1))
                        ==> next == pins + 1) by(bit_vector);
                }

                pub(crate) proof fn zero_is_terminal()
                    ensures acquire_spec(0) == Acquire::Zero,
                        release_spec(0) == Release::FailStop,
                {}

                pub(crate) proof fn overflow_is_distinct_from_zero()
                    ensures acquire_spec(<$word>::MAX) == Acquire::Overflow,
                {}

                pub(crate) proof fn final_release_is_unique(previous: $word)
                    ensures (release_spec(previous) == Release::LastPin) <==> previous == 1,
                        (release_spec(previous) == Release::StillPinned) <==> previous > 1,
                {}
            }
        }
    };
}

#[cfg(any(verus_only, target_pointer_width = "32"))]
width!(word32, u32);
#[cfg(any(verus_only, target_pointer_width = "64"))]
width!(word64, u64);

#[cfg(all(not(verus_only), target_pointer_width = "32"))]
pub(crate) use word32::{acquire, release};
#[cfg(all(not(verus_only), target_pointer_width = "64"))]
pub(crate) use word64::{acquire, release};

/// Control flow after an index observation, shared with the ownership proof.
/// Rollback captures its domain before leaving admission; reclamation follows leave.
macro_rules! lookup_after_observation {
    (eligible = $eligible:expr, acquire = $acquire:expr, resident = $resident:expr,
     rollback $retired:ident = $rollback:expr, context $context:ident = $capture:expr,
     leave = $leave:expr, reclaim = $reclaim:expr, success = $success:expr,
     overflow = $overflow:expr $(,)?) => {{
        if !$eligible {
            $leave;
            None
        } else {
            match $acquire {
                Ok(true) => {
                    if !$resident {
                        let $retired = $rollback;
                        let $context = $capture;
                        $leave;
                        $reclaim;
                        None
                    } else {
                        $leave;
                        $success
                    }
                }
                Ok(false) => {
                    $leave;
                    None
                }
                Err(_) => $overflow,
            }
        }
    }};
}
pub(crate) use lookup_after_observation;

#[cfg(verus_only)]
pub(crate) use vstd::prelude::verus_exec_expr as pin_retry_expr;
#[cfg(not(verus_only))]
macro_rules! pin_retry_expr { ($($body:tt)*) => { $($body)* }; }
#[cfg(not(verus_only))]
pub(crate) use pin_retry_expr;

/// fetch_update's retry protocol: load once, then reuse the failed CAS value.
/// The backend supplies a weak CAS with its native ordering and proof resources.
macro_rules! acquire_retry {
    ($raw:ident, $next:ident; load = $load:expr, classify = $classify:expr,
     attempt = $attempt:expr, success = $success:expr,
     zero = $zero:expr, overflow = $overflow:expr; $($annotations:tt)*) => {
        pin_retry_expr!({
            let mut $raw = $load;
            loop $($annotations)* {
                match $classify {
                    Acquire::Zero => return $zero,
                    Acquire::Overflow => return $overflow,
                    Acquire::Acquired($next) => {
                        match $attempt {
                            Ok(_) => return $success,
                            Err(current) => { $raw = current; }
                        }
                    }
                }
            }
        })
    };
}
pub(crate) use acquire_retry;

/// Final-pin ordering shared by native release, Loom, and the token proof.
/// The fence's weak-memory effect is validated by Loom, not by the SeqCst proof.
macro_rules! release_pin {
    ($previous:ident; decrement = $decrement:expr, classify = $classify:expr,
     fence = $fence:expr, last = $last:expr, pinned = $pinned:expr,
     fail_stop = $fail_stop:expr $(,)?) => {
        pin_retry_expr!({
            let $previous = $decrement;
            match $classify {
                Release::FailStop => $fail_stop,
                Release::StillPinned => $pinned,
                Release::LastPin => {
                    $fence;
                    $last
                }
            }
        })
    };
}
pub(crate) use release_pin;
