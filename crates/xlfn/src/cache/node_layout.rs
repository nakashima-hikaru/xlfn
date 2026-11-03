//! Field declaration and inline value projection shared with the memory proof.
#[cfg(not(verus_only))]
macro_rules! verus { ($($item:tt)*) => { $($item)* }; }
#[cfg(not(verus_only))]
pub(crate) use verus;

macro_rules! declare_node {
    ($vis:vis struct $name:ident<$value:ident> {
        pins: $pins:ty, resident: $resident:ty, domain: $domain:ty $(,)?
    }) => {
        verus! {
            $vis struct $name<$value> {
                $vis value: $value,
                $vis pins: $pins,
                $vis resident: $resident,
                // Immutable: only zero-budget nodes bypass index publication.
                $vis published: bool,
                $vis weight: u64,
                $vis generation: u64,
                $vis domain: $domain,
            }
        }
    };
}
pub(crate) use declare_node;

macro_rules! value {
    ($node:expr) => {
        &($node).value
    };
}
pub(crate) use value;

// The immutable allocation owner is captured while allocation access is valid.
macro_rules! domain {
    ($node:expr) => {
        ($node).domain
    };
}
pub(crate) use domain;

macro_rules! generation {
    ($node:expr) => {
        ($node).generation
    };
}
pub(crate) use generation;
macro_rules! eligible {
    ($generation:expr, $epoch:expr, $resident:expr) => {
        $generation == $epoch && $resident
    };
}
pub(crate) use eligible;

macro_rules! resident {
    ($node:expr) => {
        ($node).resident.load(std::sync::atomic::Ordering::Acquire)
    };
}
pub(crate) use resident;
