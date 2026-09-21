//! Ownership-moving queue operations shared with the executable verifier.
macro_rules! append_retired {
    ($queue:expr, $payload:expr) => {{
        ($queue).push($payload);
    }};
}
pub(crate) use append_retired;

macro_rules! take_retired {
    ($queue:expr) => {{
        let mut records = Default::default();
        core::mem::swap($queue, &mut records);
        records
    }};
}
pub(crate) use take_retired;

// Identity is checked before moving any payload between owner-bound batches.
macro_rules! append_owned_batch {
    ($left:expr, $right:expr; $reject:expr, $append:expr) => {{
        let left: *const _ = $left;
        let right: *const _ = $right;
        if left != right {
            $reject;
        }
        $append
    }};
}
pub(crate) use append_owned_batch;
