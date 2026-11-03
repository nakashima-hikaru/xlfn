//! Append-only call ownership, shared with the Verus retained-permit proof.
#[cfg(verus_only)]
use vstd::prelude::*;
#[cfg(not(verus_only))]
macro_rules! verus { ($($item:tt)*) => { $($item)* }; }
verus! {
pub(crate) enum Retained<P> { Empty, Single(P), Multiple(Vec<P>) }
}
macro_rules! retain_logic {
    ($slot:ident, $permit:ident) => {{
        // Reserve while all existing permits still belong to the caller.
        // Once movement starts every next push fits, and exhaustive matching
        // transports ownership without a second, allegedly impossible phase.
        let mut first_pair = match $slot {
            Retained::Single(_) => Vec::with_capacity(2),
            Retained::Multiple(list) => {
                list.reserve(1);
                Vec::new()
            }
            Retained::Empty => Vec::new(),
        };
        let mut previous = Retained::Empty;
        core::mem::swap($slot, &mut previous);
        *$slot = match previous {
            Retained::Empty => Retained::Single($permit),
            Retained::Single(existing) => {
                first_pair.push(existing);
                first_pair.push($permit);
                Retained::Multiple(first_pair)
            }
            Retained::Multiple(mut list) => {
                list.push($permit);
                Retained::Multiple(list)
            }
        };
    }};
}
#[cfg(not(verus_only))]
impl<P> Retained<P> {
    #[inline]
    pub(crate) fn insert(&mut self, permit: P) {
        retain_logic!(self, permit);
    }
}
#[cfg(verus_only)]
verus! {
impl<P> Retained<P> {
    pub(crate) open spec fn view(&self) -> Seq<P> {
        match self { Retained::Empty => Seq::empty(), Retained::Single(p) => seq![*p], Retained::Multiple(list) => list@ }
    }
    pub(crate) fn insert(&mut self, permit: P)
        ensures final(self).view() == old(self).view().push(permit),
    { retain_logic!(self, permit); }
}
}
