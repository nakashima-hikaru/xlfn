//! Executable ordering model backed by the shared machine-word transitions.
//! Reader release is explicit; no wait/close operation sets a live count to zero.
//! Synchronization adapter refinement and full path composition are separate
//! obligations recorded in REFINEMENT_WORKLIST.md.

use vstd::prelude::*;
use super::protocol::*;

use super::transitions::*;
use super::gate_permits::admission;

macro_rules! verify_width {
    ($module:ident, $word:ty, $sealed:ident, $mask:ident,
     $release:ident, $reopen:ident, $acquire:ident) => {
    pub mod $module {
    use super::*;
verus! {

pub struct Rotation {
    pub domain: *const u8,
    pub zero: $word,
    pub one: $word,
    pub current: bool,
    pub pending: Option<bool>,
    pub locked: bool,
    pub barrier: bool,
    pub closed: bool,
    pub observed_idle: bool,
    pub callback_done: bool,
}

/// Environment snapshots overapproximate interference before every gate RMW.
/// Every snapshot must satisfy the generation invariant; history preservation
/// and the production atomic adapter remain separate composition obligations.
pub struct Reader {
    pub rotation: Rotation,
    pub samples: Vec<Rotation>,
    pub admitted: Option<bool>,
    pub closure_seen: bool,
}
impl Reader {
    pub open spec fn inv(&self) -> bool {
        self.rotation.inv()
        && forall|i: int| 0 <= i < self.samples.len() ==> #[trigger] self.samples[i].inv()
    }
    fn is_closed(&mut self) -> (closed: bool)
        requires old(self).inv(),
        ensures final(self).inv(),
            final(self).admitted == old(self).admitted,
            closed == final(self).closure_seen,
    {
        self.closure_seen = self.rotation.closed;
        self.closure_seen
    }
    fn select(&self) -> (selected: bool)
        ensures selected == self.rotation.current,
    { self.rotation.current }

    #[verifier::exec_allows_no_decreases_clause]
    fn acquire(&mut self, selected: bool) -> (result: Result<(), ()>)
        requires old(self).inv(), old(self).admitted.is_none(),
        ensures final(self).inv(),
            result.is_ok() ==> final(self).admitted == Some(selected)
                && selected == final(self).rotation.current,
            result.is_err() ==> final(self).admitted.is_none(),
    {
        if self.samples.len() > 0 {
            self.rotation = self.samples.pop().unwrap();
        }
        match self.rotation.reader_acquire_selected(selected) {
            TransitionOutcome::Success(_) => {
                self.admitted = Some(selected);
                Ok(())
            },
            TransitionOutcome::Rejected => Err(()),
            // Native acquire aborts on overflow; there is no returning path.
            TransitionOutcome::FailStop => { loop {} },
        }
    }
    fn permit(&self, selected: bool) -> (permit: bool)
        requires self.admitted == Some(selected), selected == self.rotation.current,
        ensures permit == selected,
    { selected }
}

#[verifier::exec_allows_no_decreases_clause]
pub fn shared_reader_entry(reader: &mut Reader) -> (result: Result<bool, ()>)
    requires old(reader).inv(), old(reader).admitted.is_none(),
    ensures final(reader).inv(),
        match result {
            Ok(selected) => final(reader).admitted == Some(selected)
                && selected == final(reader).rotation.current,
            Err(_) => final(reader).admitted.is_none() && final(reader).closure_seen,
        },
{
    enter_reader!(selected;
        reader.is_closed(), reader.select(), (), reader.acquire(selected),
        reader.permit(selected), (), ();
        invariant reader.inv(), reader.admitted.is_none(),
    )
}

/// Owns the active-count ledgers for the two gate instances represented by
/// this rotation model. The instances remain fixed across generation reuse.
pub tracked struct GenerationLedgers {
    pub zero: admission::Instance,
    pub one: admission::Instance,
    pub active_zero: admission::active,
    pub active_one: admission::active,
}
impl GenerationLedgers {
    pub open spec fn matches(&self, rotation: &Rotation) -> bool {
        self.zero.id() != self.one.id()
        && self.active_zero.instance_id() == self.zero.id()
        && self.active_one.instance_id() == self.one.id()
        && self.active_zero.value() == (rotation.zero & $mask) as nat
        && self.active_one.value() == (rotation.one & $mask) as nat
    }
    pub open spec fn selected_id(&self, selected: bool) -> vstd::tokens::InstanceId {
        if selected { self.one.id() } else { self.zero.id() }
    }
}

impl Rotation {
    pub open spec fn gate(&self, which: bool) -> $word {
        if which { self.one } else { self.zero }
    }

    pub open spec fn sealed(&self, which: bool) -> bool {
        self.gate(which) & $sealed != 0
    }

    pub open spec fn idle(&self, which: bool) -> bool {
        self.gate(which) & $mask == 0
    }

    pub open spec fn inv(&self) -> bool {
        &&& (!self.sealed(false) ==> !self.current)
        &&& (!self.sealed(true) ==> self.current)
        &&& match self.pending {
            Some(old) => self.sealed(old),
            None => self.sealed(!self.current) && self.idle(!self.current),
        }
        &&& (self.observed_idle ==> self.pending.is_some()
            && self.idle(self.pending.unwrap()))
        &&& (self.callback_done ==> self.observed_idle)
    }

    fn seal_current(&mut self)
        requires old(self).inv(), old(self).locked, old(self).barrier,
            !old(self).closed, old(self).pending.is_none(),
        ensures final(self).inv(), final(self).domain == old(self).domain,
            final(self).zero & $mask == old(self).zero & $mask,
            final(self).one & $mask == old(self).one & $mask,
            final(self).sealed(false), final(self).sealed(true),
            final(self).idle(!final(self).current),
            final(self).current == old(self).current,
            final(self).locked, final(self).barrier, !final(self).closed,
            final(self).pending.is_none(),
    {
        let raw_zero = self.zero;
        let raw_one = self.one;
        assert((raw_zero | $sealed) & $sealed != 0) by(bit_vector);
        assert((raw_one | $sealed) & $sealed != 0) by(bit_vector);
        assert((raw_zero | $sealed) & $mask == raw_zero & $mask) by(bit_vector);
        assert((raw_one | $sealed) & $mask == raw_one & $mask) by(bit_vector);
        if self.current { self.one = self.one | $sealed; }
        else { self.zero = self.zero | $sealed; }
    }

    fn mark_pending(&mut self)
        requires old(self).inv(), old(self).locked, old(self).barrier,
            !old(self).closed, old(self).pending.is_none(),
            old(self).sealed(old(self).current),
        ensures final(self).inv(), final(self).domain == old(self).domain, final(self).pending == Some(final(self).current),
            final(self).current == old(self).current,
            final(self).zero == old(self).zero, final(self).one == old(self).one,
            final(self).locked, final(self).barrier, !final(self).closed,
    { self.pending = Some(self.current); }

    fn publish(&mut self)
        requires old(self).inv(), old(self).locked, old(self).barrier,
            !old(self).closed, old(self).pending == Some(old(self).current),
            old(self).sealed(!old(self).current), old(self).idle(!old(self).current),
        ensures final(self).inv(), final(self).domain == old(self).domain,
            final(self).zero & $mask == old(self).zero & $mask,
            final(self).one & $mask == old(self).one & $mask, final(self).current == !old(self).current,
            final(self).pending == Some(old(self).current),
            final(self).sealed(false), final(self).sealed(true),
            final(self).idle(final(self).current),
            final(self).locked, final(self).barrier, !final(self).closed,
    { self.current = !self.current; }

    fn publication_window(&mut self)
        requires old(self).inv(), old(self).locked, old(self).barrier,
            old(self).pending == Some(!old(self).current),
            old(self).sealed(false), old(self).sealed(true),
        ensures *final(self) == *old(self),
    {
        // No delayed reader can acquire either gate in this window.
    }

    fn reopen(&mut self)
        requires old(self).inv(), old(self).locked, old(self).barrier,
            !old(self).closed, old(self).pending == Some(!old(self).current),
            old(self).sealed(old(self).current), old(self).idle(old(self).current),
        ensures final(self).inv(), final(self).domain == old(self).domain,
            final(self).zero & $mask == old(self).zero & $mask,
            final(self).one & $mask == old(self).one & $mask, final(self).current == old(self).current,
            final(self).pending == old(self).pending,
            !final(self).sealed(final(self).current),
            final(self).locked, final(self).barrier, !final(self).closed,
    {
        let raw = if self.current { self.one } else { self.zero };
        match $reopen(raw) {
            TransitionOutcome::Success(next) => {
                if self.current { self.one = next; } else { self.zero = next; }
            },
            _ => { assert(false); },
        }
        assert(((0 as $word) & $sealed) == 0) by(bit_vector);
        assert(((0 as $word) & $mask) == 0) by(bit_vector);
    }

    fn release_barrier(&mut self)
        requires old(self).inv(), old(self).locked, old(self).barrier,
            old(self).pending == Some(!old(self).current),
            !old(self).sealed(old(self).current),
        ensures final(self).inv(), final(self).domain == old(self).domain,
            final(self).zero & $mask == old(self).zero & $mask,
            final(self).one & $mask == old(self).one & $mask, !final(self).barrier, final(self).locked,
            final(self).current == old(self).current,
            final(self).pending == old(self).pending,
    { self.barrier = false; }

    /// The reader may have selected this gate arbitrarily long ago. The actual
    /// counter transition, rather than an extra current-index check, excludes
    /// admission to an unpublished/reclaimed generation.
    pub fn reader_acquire_selected(&mut self, selected: bool) -> (result: TransitionOutcome<$word>)
        requires old(self).inv(),
        ensures final(self).inv(), final(self).domain == old(self).domain,
            final(self).current == old(self).current,
            match result {
                TransitionOutcome::Success(next) => selected == old(self).current
                    && final(self).gate(selected) == next
                    && final(self).gate(!selected) == old(self).gate(!selected)
                    && (next & $mask) == (old(self).gate(selected) & $mask) + 1,
                _ => final(self).zero == old(self).zero && final(self).one == old(self).one,
            },
    {
        let raw = if selected { self.one } else { self.zero };
        let result = $acquire(raw);
        match result {
            TransitionOutcome::Success(next) => {
                assert((raw & $mask != $mask && next == raw.wrapping_add(1)) ==>
                    ((next & $mask) == (raw & $mask) + 1
                    && (next & $sealed) == (raw & $sealed))) by(bit_vector);
                if selected { self.one = next; } else { self.zero = next; }
            },
            _ => {},
        }
        result
    }

    /// Connect the exact selected-gate RMW to a linear permit from that
    /// generation's fixed ledger. A stale selection cannot produce a permit.
    pub fn acquire_selected_with_permit(&mut self, selected: bool,
        Tracked(ledgers): Tracked<&mut GenerationLedgers>,
    ) -> (result: (TransitionOutcome<$word>, Tracked<Option<admission::permits>>))
        requires old(self).inv(), old(ledgers).matches(old(self)),
        ensures final(self).inv(), final(self).domain == old(self).domain, final(ledgers).matches(final(self)),
            final(ledgers).zero == old(ledgers).zero, final(ledgers).one == old(ledgers).one,
            final(self).current == old(self).current,
            match result.0 {
                TransitionOutcome::Success(_) => selected == final(self).current
                    && result.1@.is_some()
                    && result.1@.unwrap().instance_id() == final(ledgers).selected_id(selected),
                _ => result.1@.is_none(),
            },
    {
        let outcome = self.reader_acquire_selected(selected);
        let tracked mut permit = None;
        if let TransitionOutcome::Success(_) = outcome {
            proof {
                if selected {
                    permit = Some(ledgers.one.acquire(&mut ledgers.active_one));
                } else {
                    permit = Some(ledgers.zero.acquire(&mut ledgers.active_zero));
                }
            }
        }
        (outcome, Tracked(permit))
    }

    /// Release consumes the permit from exactly the selected generation, then
    /// preserves both fixed instance mappings and their decoded counter counts.
    pub fn release_selected_permit(&mut self, selected: bool,
        Tracked(ledgers): Tracked<&mut GenerationLedgers>,
        Tracked(permit): Tracked<admission::permits>,
    )
        requires old(self).inv(), old(ledgers).matches(old(self)),
            permit.instance_id() == old(ledgers).selected_id(selected),
        ensures final(self).inv(), final(self).domain == old(self).domain, final(ledgers).matches(final(self)),
            final(ledgers).zero == old(ledgers).zero, final(ledgers).one == old(ledgers).one,
            final(self).current == old(self).current,
    {
        proof {
            if selected { ledgers.one.positive(&ledgers.active_one, &permit); }
            else { ledgers.zero.positive(&ledgers.active_zero, &permit); }
        }
        self.reader_release(selected);
        proof {
            if selected { ledgers.one.release(&mut ledgers.active_one, permit); }
            else { ledgers.zero.release(&mut ledgers.active_zero, permit); }
        }
    }

    /// A drained generation cannot still own a matching linear admission.
    pub proof fn idle_excludes_generation_permit(&self, selected: bool,
        tracked ledgers: &GenerationLedgers, tracked permit: &admission::permits)
        requires ledgers.matches(self), self.idle(selected),
            permit.instance_id() == ledgers.selected_id(selected),
        ensures false,
    {
        if selected { ledgers.one.positive(&ledgers.active_one, permit); }
        else { ledgers.zero.positive(&ledgers.active_zero, permit); }
    }

    /// Environment reader releases execute the actual counter transition.
    /// This is an enabled step, not a wait axiom that erases outstanding readers.
    pub fn reader_release(&mut self, which: bool)
        requires old(self).inv(), !old(self).idle(which),
        ensures final(self).inv(), final(self).domain == old(self).domain,
            final(self).gate(which) & $mask
                == (old(self).gate(which) & $mask) - 1,
            final(self).gate(!which) == old(self).gate(!which),
            final(self).current == old(self).current,
            final(self).pending == old(self).pending,
    {
        let raw = if which { self.one } else { self.zero };
        let next = $release(raw);
        match next {
            TransitionOutcome::Success(value) => {
                assert((raw & $mask != 0
                    && value == raw.wrapping_sub(1)) ==>
                    ((value & $mask) == (raw & $mask) - 1
                    && (value & $sealed) == (raw & $sealed))) by(bit_vector);
                if which { self.one = value; } else { self.zero = value; }
            },
            _ => { assert(false); },
        }
    }

    pub fn observe_pending_idle(&mut self)
        requires old(self).inv(), old(self).locked, !old(self).barrier,
            old(self).pending == Some(!old(self).current),
            old(self).idle(!old(self).current),
        ensures final(self).inv(), final(self).domain == old(self).domain, final(self).observed_idle,
            final(self).pending == old(self).pending,
            final(self).locked, !final(self).barrier,
    { self.observed_idle = true; }

    fn try_observe_pending_idle(&mut self) -> (idle: bool)
        requires old(self).inv(), old(self).locked, !old(self).barrier,
            old(self).pending == Some(!old(self).current),
            !old(self).observed_idle, !old(self).callback_done,
        ensures final(self).inv(), final(self).domain == old(self).domain, final(self).locked, !final(self).barrier,
            final(self).pending == old(self).pending,
            final(self).current == old(self).current,
            !final(self).callback_done,
            final(self).observed_idle == idle,
            idle == final(self).idle(!final(self).current),
            final(self).zero == old(self).zero, final(self).one == old(self).one,
    {
        let raw = if self.current { self.zero } else { self.one };
        let idle = raw & $mask == 0;
        self.observed_idle = idle;
        idle
    }

    fn callback(&mut self)
        requires old(self).inv(), old(self).locked, !old(self).barrier,
            old(self).observed_idle, !old(self).callback_done,
        ensures final(self).inv(), final(self).domain == old(self).domain, final(self).callback_done,
            final(self).pending == old(self).pending,
            final(self).current == old(self).current,
            final(self).locked, !final(self).barrier,
    { self.callback_done = true; }

    fn clear_pending(&mut self)
        requires old(self).inv(), old(self).locked, !old(self).barrier,
            old(self).callback_done,
            old(self).pending == Some(!old(self).current),
        ensures final(self).inv(), final(self).domain == old(self).domain, final(self).pending.is_none(),
            !final(self).observed_idle, !final(self).callback_done,
    {
        self.pending = None;
        self.observed_idle = false;
        self.callback_done = false;
    }

    fn close(&mut self)
        requires old(self).inv(), old(self).locked, !old(self).barrier,
        ensures final(self).inv(), final(self).domain == old(self).domain, final(self).locked, final(self).closed,
            final(self).zero == old(self).zero, final(self).one == old(self).one,
            final(self).current == old(self).current,
    { self.closed = true; }

    fn seal_for_close(&mut self, which: bool)
        requires old(self).inv(), old(self).locked, old(self).closed,
        ensures final(self).inv(), final(self).domain == old(self).domain, final(self).locked, final(self).closed,
            final(self).sealed(which),
            final(self).gate(which) & $mask
                == old(self).gate(which) & $mask,
            final(self).gate(!which) == old(self).gate(!which),
    {
        let raw = if which { self.one } else { self.zero };
        let next = raw | $sealed;
        assert((raw | $sealed) & $mask == raw & $mask) by(bit_vector);
        assert((raw | $sealed) & $sealed != 0) by(bit_vector);
        if which { self.one = next; } else { self.zero = next; }
    }
}

/// Representation relation to the abstract generation protocol. Permit counts
/// are explicit parameters: they are not invented or discharged by a wait.
pub open spec fn view(model: Rotation, p0: nat, p1: nat) -> super::super::RotatingDomainState {
    super::super::RotatingDomainState {
        gen0: super::super::GenState {
            sealed: model.sealed(false), active: (model.zero & $mask) as nat, permits: p0,
        },
        gen1: super::super::GenState {
            sealed: model.sealed(true), active: (model.one & $mask) as nat, permits: p1,
        },
        current: if model.current { 1 } else { 0 },
        transition_pending: match model.pending {
            Some(old) => Some(if old { 1 } else { 0 }),
            None => None,
        },
        closed: model.closed,
        close_complete: false,
    }
}

pub proof fn representation_preserves_generation_invariants(model: Rotation, p0: nat, p1: nat)
    requires model.inv(), p0 <= (model.zero & $mask) as nat, p1 <= (model.one & $mask) as nat,
    ensures super::super::rotating_domain_inv(view(model, p0, p1)),
{}

pub proof fn observed_pending_has_no_permits(model: Rotation, p0: nat, p1: nat)
    requires model.inv(), model.observed_idle,
        p0 <= (model.zero & $mask) as nat, p1 <= (model.one & $mask) as nat,
    ensures match model.pending {
        Some(false) => p0 == 0,
        Some(true) => p1 == 0,
        None => false,
    },
{}

pub fn shared_begin_and_publication(model: &mut Rotation)
    requires old(model).inv(), old(model).locked, old(model).barrier,
        !old(model).closed, old(model).pending.is_none(),
    ensures final(model).inv(), final(model).domain == old(model).domain, final(model).current == !old(model).current,
        final(model).zero & $mask == old(model).zero & $mask,
        final(model).one & $mask == old(model).one & $mask,
        final(model).pending == Some(old(model).current),
        final(model).sealed(old(model).current),
        final(model).locked, !final(model).barrier,
{
    begin_rotation!(model.seal_current(), model.mark_pending(),
        publish_release!(
            publish_reopen!(model.publish(), model.publication_window(), model.reopen()),
            model.release_barrier()));
}

pub fn shared_finish(model: &mut Rotation)
    requires old(model).inv(), old(model).locked, !old(model).barrier,
        old(model).pending == Some(!old(model).current),
        old(model).observed_idle, !old(model).callback_done,
    ensures final(model).inv(), final(model).pending.is_none(),
{
    finish_rotation!(result; model.callback(), model.clear_pending());
}

/// No zero-count precondition: a polled finish must refuse live readers.
pub fn shared_try_finish(model: &mut Rotation) -> (result: Option<Result<(), ()>>)
    requires old(model).inv(), old(model).locked, !old(model).barrier,
        old(model).pending == Some(!old(model).current),
        !old(model).observed_idle, !old(model).callback_done,
    ensures final(model).inv(),
        result.is_some() ==> final(model).pending.is_none(),
        result.is_none() ==> final(model).pending == old(model).pending,
        !old(model).idle(!old(model).current) ==> result.is_none(),
{
    try_finish_rotation!(model.try_observe_pending_idle(),
        finish_rotation!(result; model.callback(), model.clear_pending()))
}

/// Close ordering only. Draining is intentionally not faked by either effect;
/// reader releases and idle observations are required before reclaiming.
pub fn shared_close_sealing(model: &mut Rotation)
    requires old(model).inv(), old(model).locked, !old(model).barrier,
    ensures final(model).inv(), final(model).closed,
        final(model).sealed(false), final(model).sealed(true),
{
    close_domain!(model.close(), model.seal_for_close(false), model.seal_for_close(true));
}

} // verus!

    }
    };
}

verify_width!(word32, u32, SEALED_BIT_32, ACTIVE_COUNT_MASK_32, release_step_32, reopen_step_32, acquire_step_32);
verify_width!(word64, u64, SEALED_BIT_64, ACTIVE_COUNT_MASK_64, release_step_64, reopen_step_64, acquire_step_64);
