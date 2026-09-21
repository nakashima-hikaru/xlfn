//! Append-only call ownership, shared with the Verus retained-permit proof.
#[cfg(verus_only)]
use vstd::prelude::*;
#[cfg(not(verus_only))]
macro_rules! verus { ($($item:tt)*) => { $($item)* }; }
verus! {
pub(crate) enum Retained<P> { Empty, Single(P), Multiple(Vec<P>) }
}
macro_rules! retain_logic {
    ($slot:ident, $permit:ident, $impossible:expr) => {{
        match $slot {
            Retained::Empty => *$slot = Retained::Single($permit),
            Retained::Single(_) => {
                // Allocate before temporarily moving existing ownership out.
                let mut list = Vec::with_capacity(2);
                let mut previous = Retained::Empty;
                core::mem::swap($slot, &mut previous);
                match previous {
                    Retained::Single(existing) => {
                        list.push(existing);
                        list.push($permit);
                        *$slot = Retained::Multiple(list);
                    }
                    _ => $impossible,
                }
            }
            Retained::Multiple(list) => list.push($permit),
        }
    }};
}
#[cfg(not(verus_only))]
impl<P> Retained<P> {
    #[inline]
    pub(crate) fn insert(&mut self, permit: P) {
        retain_logic!(self, permit, xlfn_kernel::invariant::fail_stop());
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
    { retain_logic!(self, permit, vstd::prelude::verus_exec_expr!({ assert(false); })); }
}
}
