//! Production/verification shared ordering for generation transitions.
//! Effects are supplied by the atomic, drain, registration, and callback backends.

macro_rules! publish_reopen {
    ($publish:expr, $between:expr, $reopen:expr) => {{
        $publish;
        $between;
        $reopen;
    }};
}

macro_rules! publish_release {
    ($publish:expr, $release_barrier:expr) => {{
        $publish;
        $release_barrier;
    }};
}

macro_rules! begin_rotation {
    ($seal:expr, $pending:expr, $publish:expr) => {{
        $seal;
        $pending;
        $publish;
    }};
}

macro_rules! finish_rotation {
    ($result:ident; $operation:expr, $clear:expr) => {{
        // The callback may unwind. Clearing pending first would allow reuse
        // without completing retirement on the next maintenance attempt.
        let $result = $operation;
        $clear;
        $result
    }};
}

macro_rules! close_domain {
    ($close:expr, $drain_first:expr, $drain_second:expr) => {{
        $close;
        $drain_first;
        $drain_second;
    }};
}

macro_rules! try_finish_rotation {
    ($observe:expr, $finish:expr) => {{
        if !$observe {
            return None;
        }
        Some(Ok($finish))
    }};
}

pub(crate) use begin_rotation;
pub(crate) use close_domain;
pub(crate) use finish_rotation;
pub(crate) use publish_release;
pub(crate) use publish_reopen;
pub(crate) use try_finish_rotation;

#[cfg(verus_only)]
pub(crate) use vstd::prelude::verus_exec_expr as protocol_expr;
#[cfg(not(verus_only))]
macro_rules! protocol_expr { ($($body:tt)*) => { $($body)* }; }
#[cfg(not(verus_only))]
pub(crate) use protocol_expr;

macro_rules! register_retired {
    ($generation:ident, $guard:ident;
     $select:expr, $lock:expr, $current:expr, $unlock:expr, $retry:expr, $append:expr;
     $($annotations:tt)*) => { protocol_expr!({
        loop
            $($annotations)*
        {
            let $generation = $select;
            let $guard = $lock;
            if $current != $generation {
                $unlock;
                $retry;
                continue;
            }
            return $append;
        }
    })};
}
pub(crate) use register_retired;

// Select again after rejected admission: the selected generation can become
// stale before the gate RMW. Construct a permit only on successful acquisition.
macro_rules! enter_reader {
    ($generation:ident;
     $closed:expr, $select:expr, $after_select:expr, $acquire:expr,
     $permit:expr, $closed_error:expr, $retry:expr;
     $($annotations:tt)*) => { protocol_expr!({
        loop
            $($annotations)*
        {
            if $closed { return Err($closed_error); }
            let $generation = $select;
            $after_select;
            match $acquire {
                Ok(()) => return Ok($permit),
                Err(_) => {
                    if $closed { return Err($closed_error); }
                    $retry;
                },
            }
        }
    })};
}
pub(crate) use enter_reader;

// Both rotating and terminal certificates authorize only their issuing domain.
macro_rules! authorize_domain {
    ($issuer:expr, $owner:expr, $payload:expr) => {{
        let issuer: *const _ = $issuer;
        let owner: *const _ = $owner;
        if issuer == owner {
            Some($payload)
        } else {
            None
        }
    }};
}
pub(crate) use authorize_domain;

macro_rules! take_authorized_queue {
    ($index:ident, $guard:ident; $authorize:expr, $lock:expr, $take:expr) => {{
        match $authorize {
            Some($index) => {
                let $guard = $lock;
                Some($take)
            }
            None => None,
        }
    }};
}
pub(crate) use take_authorized_queue;

macro_rules! take_authorized_queues {
    ($indices:ident, $first:ident, $second:ident;
     $authorize:expr, $lock_first:expr, $lock_second:expr, $take:expr) => {{
        match $authorize {
            Some($indices) => {
                let $first = $lock_first;
                let $second = $lock_second;
                Some($take)
            }
            None => None,
        }
    }};
}
pub(crate) use take_authorized_queues;
