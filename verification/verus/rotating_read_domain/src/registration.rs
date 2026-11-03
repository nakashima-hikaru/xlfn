//! Executable queue-registration backend for shared production retry control.
//! Interference is allowed between selecting a generation and taking its queue
//! lock. Holding that queue excludes publication through the same barrier.
use vstd::prelude::*;
use super::protocol::protocol_expr;
verus! {
pub struct Registration<P> {
    pub owner: *const u8,
    pub both_held: bool,
    pub zero: Vec<P>,
    pub one: Vec<P>,
    pub current: bool,
    pub held: Option<bool>,
    pub registered: Option<bool>,
    pub samples: Vec<bool>,
    pub cursor: usize,
}
impl<P> Registration<P> {
    pub fn lock_terminal(&mut self, index: usize) -> (guard: usize)
        requires !old(self).both_held,
            (index == 0 && old(self).held.is_none())
            || (index == 1 && old(self).held == Some(false)),
        ensures guard == index, final(self).held == Some(false),
            final(self).both_held == (index == 1),
            final(self).zero@ == old(self).zero@, final(self).one@ == old(self).one@,
            final(self).owner == old(self).owner,
    {
        if index == 0 { self.held = Some(false); }
        else { self.both_held = true; }
        index
    }

    pub fn take_pair(&mut self, first: usize, second: usize) -> (batches: [Vec<P>; 2])
        requires first == 0, second == 1, old(self).held == Some(false), old(self).both_held,
        ensures batches[0]@ == old(self).zero@, batches[1]@ == old(self).one@,
            final(self).zero@ == Seq::<P>::empty(), final(self).one@ == Seq::<P>::empty(),
            final(self).held.is_none(), !final(self).both_held, final(self).owner == old(self).owner,
    {
        let mut zero = Vec::new();
        let mut one = Vec::new();
        core::mem::swap(&mut self.zero, &mut zero);
        core::mem::swap(&mut self.one, &mut one);
        self.held = None;
        self.both_held = false;
        [zero, one]
    }

    pub fn lock_drain(&mut self, index: usize) -> (guard: usize)
        requires old(self).held.is_none() && !old(self).both_held, index < 2,
        ensures final(self).both_held == old(self).both_held, final(self).held == Some(index == 1), guard == index,
            final(self).zero@ == old(self).zero@, final(self).one@ == old(self).one@,
            final(self).owner == old(self).owner,
    { self.held = Some(index == 1); index }

    pub fn take_locked(&mut self, index: usize, guard: usize) -> (entries: Vec<P>)
        requires index < 2, guard == index, old(self).held == Some(index == 1),
        ensures final(self).both_held == old(self).both_held, final(self).held.is_none(), final(self).owner == old(self).owner,
            entries@ == if index == 1 { old(self).one@ } else { old(self).zero@ },
            final(self).zero@ == if index == 1 { old(self).zero@ } else { Seq::empty() },
            final(self).one@ == if index == 1 { Seq::empty() } else { old(self).one@ },
    {
        let mut entries = Vec::new();
        if index == 1 { core::mem::swap(&mut self.one, &mut entries); }
        else { core::mem::swap(&mut self.zero, &mut entries); }
        self.held = None;
        entries
    }

    fn current(&self) -> (generation: bool)
        ensures generation == self.current,
    { self.current }

    fn lock(&mut self, selected: bool)
        requires old(self).held.is_none() && !old(self).both_held, old(self).registered.is_none(),
            old(self).cursor <= old(self).samples.len(),
        ensures final(self).both_held == old(self).both_held, final(self).owner == old(self).owner, final(self).held == Some(selected), final(self).registered.is_none(),
            final(self).cursor <= final(self).samples.len(),
            final(self).zero@ == old(self).zero@, final(self).one@ == old(self).one@,
    {
        if self.cursor < self.samples.len() {
            self.current = self.samples[self.cursor];
            self.cursor += 1;
        }
        self.held = Some(selected);
    }

    fn unlock(&mut self, _guard: ())
        requires old(self).held.is_some(), old(self).registered.is_none(),
        ensures final(self).both_held == old(self).both_held, final(self).owner == old(self).owner, final(self).held.is_none(), final(self).registered.is_none(),
            final(self).cursor == old(self).cursor, final(self).samples == old(self).samples,
            final(self).zero@ == old(self).zero@, final(self).one@ == old(self).one@,
    { self.held = None; }

    fn append(&mut self, selected: bool, _guard: (), payload: P) -> (registered: bool)
        requires old(self).held == Some(selected), selected == old(self).current,
            old(self).registered.is_none(),
        ensures final(self).both_held == old(self).both_held, final(self).owner == old(self).owner, final(self).registered == Some(selected), final(self).held.is_none(),
            registered == selected, final(self).current == old(self).current,
            final(self).zero@ == if selected { old(self).zero@ } else { old(self).zero@.push(payload) },
            final(self).one@ == if selected { old(self).one@.push(payload) } else { old(self).one@ },
    {
        if selected { super::queue_transitions::append_retired!(&mut self.one, payload); } else { super::queue_transitions::append_retired!(&mut self.zero, payload); }
        self.registered = Some(selected);
        self.held = None;
        selected
    }
}

#[verifier::exec_allows_no_decreases_clause]
pub fn shared_registration<P>(model: &mut Registration<P>, payload: P) -> (registered: bool)
    requires old(model).held.is_none() && !old(model).both_held, old(model).registered.is_none(),
        old(model).cursor <= old(model).samples.len(),
    ensures final(model).both_held == old(model).both_held, final(model).owner == old(model).owner, final(model).held.is_none(), final(model).registered == Some(registered),
        registered == final(model).current,
        final(model).zero@ == if registered { old(model).zero@ } else { old(model).zero@.push(payload) },
        final(model).one@ == if registered { old(model).one@.push(payload) } else { old(model).one@ },
{
    super::protocol::register_retired!(selected, guard;
        model.current(), model.lock(selected), model.current(), model.unlock(guard), (),
        model.append(selected, guard, payload);
        invariant model.held.is_none(), !model.both_held, model.registered.is_none(),
            model.cursor <= model.samples.len(),
            model.zero@ == old(model).zero@, model.one@ == old(model).one@,
            model.owner == old(model).owner, model.both_held == old(model).both_held,
    )
}
}
