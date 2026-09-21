//! Shared destruction-completion ordering; effects retain their backend semantics.
macro_rules! complete_reclamation {
    ($receipt:ident, $guard:ident;
     $destroy:expr, $discharge:expr, $lock:expr, $notify:expr, $unlock:expr) => {{
        let $receipt = $destroy;
        $discharge;
        let $guard = $lock;
        $notify;
        $unlock;
    }};
}
pub(crate) use complete_reclamation;

// Identity is checked before moving any payload between owner-bound batches.
macro_rules! append_owned_batch {
    ($left:expr, $right:expr; $reject:expr, $append:expr) => {{
        let left: *const _ = $left;
        let right: *const _ = $right;
        if left != right {
            $reject;
        }
        $append;
    }};
}
pub(crate) use append_owned_batch;
