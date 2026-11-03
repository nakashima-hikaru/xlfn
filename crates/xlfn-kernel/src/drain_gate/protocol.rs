//! Shared control flow for the production, Loom, and Verus drain protocols.
//!
//! Backend expressions supply synchronization effects. Verus checks their order
//! against an executable model; sync/atomic semantics remain in the TCB.
//! The optional loop annotations contain proof metadata only.

#[cfg(verus_only)]
pub(crate) use vstd::prelude::verus_exec_expr as protocol_expr;

#[cfg(not(verus_only))]
macro_rules! protocol_expr {
    ($($body:tt)*) => { $($body)* };
}
#[cfg(not(verus_only))]
pub(crate) use protocol_expr;

macro_rules! release_tail {
    ($guard:ident, $outcome:ident;
     $lock:expr, $release:expr, $notify:expr, $unlock:expr) => {{
        let $guard = $lock;
        let $outcome = $release;
        if let ReleaseOutcome::BecameIdle = $outcome {
            $notify;
        }
        $unlock;
        $outcome
    }};
}

macro_rules! wait_loop {
    ($guard:ident, $active:ident;
     $lock:expr, $observe:expr, $wait:expr, $unlock:expr;
     $($annotations:tt)*) => { protocol_expr!({
        let mut $guard = $lock;
        let mut $active = $observe;
        while $active != 0
            $($annotations)*
        {
            $guard = $wait;
            $active = $observe;
        }
        $unlock;
    })};
}

pub(crate) use release_tail;
pub(crate) use wait_loop;

// Visit every stripe so a blocked wait registers notification everywhere.
// Only zero/nonzero matters; summing independent counts can overflow usize.
macro_rules! observe_stripes {
    ($index:ident, $idle:ident; $len:expr, $observe:expr;
     $($annotations:tt)*) => { protocol_expr!({
        let mut $index = 0usize;
        let mut $idle = true;
        while $index < $len
            $($annotations)*
        {
            if $observe != 0 {
                $idle = false;
            }
            $index += 1;
        }
        $idle
    })};
}
pub(crate) use observe_stripes;

macro_rules! seal_stripes {
    ($index:ident, $rollback:ident; $len:expr, $seal:expr, $undo:expr;
     [$($outer:tt)*]; [$($inner:tt)*]) => { protocol_expr!({
        let mut $index = 0usize;
        while $index < $len
            $($outer)*
        {
            if !$seal {
                let mut $rollback = 0usize;
                while $rollback < $index
                    $($inner)*
                {
                    $undo;
                    $rollback += 1;
                }
                return false;
            }
            $index += 1;
        }
        true
    })};
}
pub(crate) use seal_stripes;

// Reopen a prefix in order; stop at the first rejected transition.
macro_rules! reopen_stripes {
    ($index:ident; $len:expr, $reopen:expr; $($annotations:tt)*) => { protocol_expr!({
        let mut $index = 0usize;
        while $index < $len
            $($annotations)*
        {
            if !$reopen { break; }
            $index += 1;
        }
        $index
    })};
}
pub(crate) use reopen_stripes;

// Unconditionally seal every stripe while retaining exclusive lifecycle control.
macro_rules! seal_all_stripes {
    ($index:ident; $len:expr, $seal:expr; $($annotations:tt)*) => { protocol_expr!({
        let mut $index = 0usize;
        while $index < $len
            $($annotations)*
        {
            $seal;
            $index += 1;
        }
    })};
}
pub(crate) use seal_all_stripes;
