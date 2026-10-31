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
