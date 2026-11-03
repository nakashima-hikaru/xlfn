//! Exclusive transition-state ownership using a verified library lock.
//! This is a resource backend, not a claim that parking_lot is this lock.
use vstd::prelude::*;
use vstd::rwlock::{RwLock, RwLockPredicate, WriteHandle};
macro_rules! width {
    ($width:ident, $mask:ident) => {
    pub mod $width {
    use super::*;
    use super::super::refinement::$width::Rotation;
    use super::super::{identity, detachment};
    use super::super::registration::Registration;
    use super::super::transitions::$mask;
    verus! {
    pub struct DomainPredicate { pub domain: *const u8 }
    impl RwLockPredicate<Rotation> for DomainPredicate {
        open spec fn inv(self, state: Rotation) -> bool {
            state.inv() && state.domain == self.domain && !state.locked
        }
    }
    pub type TransitionLock = RwLock<Rotation, DomainPredicate>;
    pub type TransitionHandle<'a> = WriteHandle<'a, Rotation, DomainPredicate>;

    pub fn new(state: Rotation) -> (lock: TransitionLock)
        requires state.inv(), !state.locked,
        ensures lock.pred().domain == state.domain,
    {
        let ghost domain = state.domain;
        RwLock::new(state, Ghost(DomainPredicate { domain }))
    }

    #[verifier::exec_allows_no_decreases_clause]
    pub fn acquire(lock: &TransitionLock) -> (result: (Rotation, TransitionHandle<'_>))
        ensures result.0.inv(), result.0.locked, result.0.domain == lock.pred().domain,
            result.1.rwlock() == *lock,
    {
        let (mut state, handle) = lock.acquire_write();
        state.locked = true;
        (state, handle)
    }

    /// Complete resource path: acquire exclusive state, issue a borrowed
    /// certificate only for idle pending work, detach, then consume the handle
    /// to return the state. No boolean flag can substitute for this handle.
    #[verifier::exec_allows_no_decreases_clause]
    pub fn try_detach_pending<P>(lock: &TransitionLock, queue: &mut Registration<P>)
        -> (result: Option<(usize, Vec<P>)>)
        requires old(queue).held.is_none(), !old(queue).both_held,
        ensures final(queue).held.is_none(), !final(queue).both_held,
            final(queue).owner == old(queue).owner,
            match result {
                Some((index, batch)) => index < 2
                    && old(queue).owner.addr() == lock.pred().domain.addr()
                    && batch@ == if index == 1 { old(queue).one@ } else { old(queue).zero@ }
                    && final(queue).zero@ == if index == 1 { old(queue).zero@ } else { Seq::empty() }
                    && final(queue).one@ == if index == 1 { Seq::empty() } else { old(queue).one@ },
                None => final(queue).zero@ == old(queue).zero@ && final(queue).one@ == old(queue).one@,
            },
    {
        let (state, handle) = acquire(lock);
        let mut result = None;
        if let Some(selected) = state.pending {
            let raw = if selected { state.one } else { state.zero };
            if raw & $mask == 0 {
                let index = if selected { 1usize } else { 0usize };
                let certificate = identity::$width::Drained::issue(state.domain, index, &state, lock, &handle);
                if let Some(batch) = detachment::$width::detach(queue, &certificate) {
                    result = Some((index, batch));
                }
            }
        }
        release(state, handle);
        result
    }

    pub fn release(mut state: Rotation, handle: TransitionHandle<'_>)
        requires state.inv(), state.locked, state.domain == handle.rwlock().pred().domain,
    {
        state.locked = false;
        handle.release_write(state);
    }
    }
    }
    };
}
width!(word32, ACTIVE_COUNT_MASK_32);
width!(word64, ACTIVE_COUNT_MASK_64);
