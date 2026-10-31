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
