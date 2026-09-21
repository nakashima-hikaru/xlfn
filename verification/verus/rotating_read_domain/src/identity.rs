//! Production-shared domain certificate authorization on actual pointers.
use vstd::prelude::*;
verus! {
pub fn authorize_generation(issuer: *const u8, owner: *const u8, index: usize) -> (result: Option<usize>)
    requires index < 2,
    ensures result == if issuer.addr() == owner.addr() { Some(index) } else { None },
{
    super::protocol::authorize_domain!(issuer, owner, index)
}
}


verus! {
pub fn authorize_closed(issuer: *const u8, owner: *const u8) -> (result: Option<[usize; 2]>)
    ensures result.is_some() == (issuer.addr() == owner.addr()),
        result.is_some() ==> result.unwrap()[0] == 0 && result.unwrap()[1] == 1,
{ super::protocol::authorize_domain!(issuer, owner, [0usize, 1usize]) }
}

macro_rules! certificate {
    ($width:ident) => {
    pub mod $width {
    use super::*;
    use super::super::refinement::$width::Rotation;
    use super::super::lock_ownership::$width::{TransitionHandle, TransitionLock};
    verus! {
    /// Borrowing the model prevents reopening/reuse while this proof
    /// certificate exists, as the native callback borrows its transition guard.
    pub struct Closed<'domain> {
        issuer: *const u8,
        state: Tracked<&'domain Rotation>,
    }
    impl<'domain> Closed<'domain> {
        pub closed spec fn inv(&self) -> bool {
            self.issuer == self.state@.domain && self.state@.inv() && self.state@.closed
            && self.state@.sealed(false) && self.state@.sealed(true)
            && self.state@.idle(false) && self.state@.idle(true)
        }
        pub closed spec fn state(&self) -> &Rotation { self.state@ }
        pub fn issue(issuer: *const u8, Tracked(state): Tracked<&'domain Rotation>) -> (closed: Self)
            requires issuer == state.domain, state.inv(), state.closed,
                state.sealed(false), state.sealed(true), state.idle(false), state.idle(true),
            ensures closed.inv(), closed.state() == state,
        { Closed { issuer, state: Tracked(state) } }
        pub fn indices_for(&self, owner: *const u8) -> (indices: Option<[usize; 2]>)
            requires self.inv(),
            ensures indices.is_some() == (owner.addr() == self.state().domain.addr()),
                indices.is_some() ==> indices.unwrap()[0] == 0 && indices.unwrap()[1] == 1,
        { super::super::protocol::authorize_domain!(self.issuer, owner, [0usize, 1usize]) }
    }

    pub struct Drained<'drain> {
        issuer: *const u8,
        index: usize,
        state: &'drain Rotation,
        handle: &'drain TransitionHandle<'drain>,
        lock: &'drain TransitionLock,
    }
    impl<'drain> Drained<'drain> {
        pub closed spec fn inv(&self) -> bool {
            self.index < 2 && self.issuer == self.state.domain
            && self.state.inv() && self.state.locked
            && self.handle.rwlock() == *self.lock
            && self.lock.pred().domain == self.state.domain
            && self.state.pending == Some(self.index == 1)
            && self.state.idle(self.index == 1)
        }
        pub closed spec fn state(&self) -> &Rotation { self.state }
        pub closed spec fn index(&self) -> usize { self.index }

        pub fn issue(issuer: *const u8, index: usize, state: &'drain Rotation,
            lock: &'drain TransitionLock, handle: &'drain TransitionHandle<'drain>)
            -> (certificate: Self)
            requires index < 2, issuer == state.domain, state.inv(), state.locked,
                state.pending == Some(index == 1), state.idle(index == 1),
                handle.rwlock() == *lock, lock.pred().domain == state.domain,
            ensures certificate.inv(), certificate.state() == state, certificate.index() == index,
        { Drained { issuer, index, state, handle, lock } }

        pub fn index_for(&self, owner: *const u8) -> (index: Option<usize>)
            requires self.inv(),
            ensures index.is_some() == (owner.addr() == self.state().domain.addr()),
                index.is_some() ==> index.unwrap() == self.index(),
                index.is_some() ==> index.unwrap() < 2
                    && self.state().idle(index.unwrap() == 1)
                    && self.state().pending == Some(index.unwrap() == 1),
        {
            super::super::protocol::authorize_domain!(self.issuer, owner, self.index)
        }
    }
    }
    }
    };
}
certificate!(word32);
certificate!(word64);
