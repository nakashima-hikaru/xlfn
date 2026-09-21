//! Executable backend models for the shared production control flow.
//!
//! Wake samples overapproximate interference: arbitrary counts and flags,
//! including spurious wakes and reopen before reacquisition. Exhausting the
//! samples repeats the current state, never fabricates idle. These are safety
//! proofs (if the wait returns), not fairness or termination proofs.

use vstd::prelude::*;
use super::transitions::*;
use super::protocol::{release_tail, wait_loop, protocol_expr};

macro_rules! verify_width {
    ($module:ident, $word:ty, $mask:ident, $waiting:ident,
     $release:ident, $release_spec:ident, $outcome:ident,
     $fast:ident, $sealed:ident, $acquire_spec:ident, $reopen_spec:ident) => {
pub(crate) mod $module {
use super::*;
verus! {

pub open spec fn decode(raw: $word, permits: nat) -> super::super::DrainGateState {
    super::super::DrainGateState {
        capacity: $mask as nat,
        sealed: raw & $sealed != 0,
        waiting: raw & $waiting != 0,
        active: (raw & $mask) as nat,
        outstanding_permits: permits,
    }
}

/// DG-5 calls the same fast transition that production passes to its RMW.
/// Rejection has no next state: the caller retains its final capability.
pub fn final_release_retains_count(raw: $word) -> (result: TransitionOutcome<$word>)
    requires raw & $waiting != 0, raw & $mask == 1,
    ensures result == TransitionOutcome::Rejected,
{ $fast(raw) }

/// DG-3/4 connects a successful wait observation to the permit model.
/// A stable reclamation certificate additionally needs seal/reopen exclusion.
pub proof fn idle_observation_has_no_permits(raw: $word, permits: nat)
    requires super::super::drain_gate_inv(decode(raw, permits)),
        raw & $mask == 0, raw & $waiting != 0,
    ensures permits == 0,
        super::super::step_wait_until_idle(decode(raw, permits)) == Some(decode(raw, permits)),
{}

pub proof fn registration_preserves_permits(raw: $word, permits: nat)
    ensures decode(raw | $waiting, permits)
        == super::super::step_mark_waiting(decode(raw, permits)),
{
    assert((raw | $waiting) & $mask == raw & $mask) by(bit_vector);
    assert((raw | $waiting) & $sealed == raw & $sealed) by(bit_vector);
    assert((raw | $waiting) & $waiting != 0) by(bit_vector);
}

pub proof fn acquire_refines_permit_model(raw: $word, next: $word, permits: nat)
    requires $acquire_spec(raw) == TransitionOutcome::Success(next),
    ensures super::super::step_acquire(decode(raw, permits))
        == Some(decode(next, permits + 1)),
{
    assert((raw & $mask) <= $mask) by(bit_vector);
    assert((raw & $mask != $mask && next == raw.wrapping_add(1)) ==>
        ((next & $mask) == (raw & $mask) + 1
        && (next & $sealed) == (raw & $sealed)
        && (next & $waiting) == (raw & $waiting))) by(bit_vector);
}

pub proof fn release_refines_permit_model(raw: $word, next: $word, permits: nat)
    requires permits > 0, $release_spec(raw) == TransitionOutcome::Success(next),
    ensures super::super::step_release(decode(raw, permits))
        == Some(decode(next, (permits - 1) as nat)),
{
    assert((raw & $mask != 0 && next == raw.wrapping_sub(1)) ==>
        ((next & $mask) == (raw & $mask) - 1
        && (next & $sealed) == (raw & $sealed)
        && (next & $waiting) == (raw & $waiting))) by(bit_vector);
}

pub proof fn reopen_refines_permit_model(raw: $word, next: $word, permits: nat)
    requires super::super::drain_gate_inv(decode(raw, permits)),
        $reopen_spec(raw) == TransitionOutcome::Success(next),
    ensures super::super::step_reopen(decode(raw, permits)) == Some(decode(next, 0)),
{
    assert((0 as $word & $mask) == 0) by(bit_vector);
    assert((0 as $word & $sealed) == 0) by(bit_vector);
    assert((0 as $word & $waiting) == 0) by(bit_vector);
}

pub struct ReleaseModel {
    pub raw: $word,
    pub held: bool,
    pub pending: bool,
    pub accessible: bool,
}

impl ReleaseModel {
    fn lock(&mut self)
        requires old(self).accessible, !old(self).held,
        ensures final(self).held, final(self).raw == old(self).raw,
            final(self).pending == old(self).pending, final(self).accessible,
    { self.held = true; }

    fn release(&mut self) -> (out: ReleaseOutcome)
        requires old(self).held, old(self).accessible,
            old(self).raw & $mask > 0,
        ensures final(self).held, final(self).accessible,
            $release_spec(old(self).raw) == TransitionOutcome::Success(final(self).raw),
            final(self).pending == (out == ReleaseOutcome::BecameIdle),
    {
        let previous = self.raw;
        let next = $release(previous);
        match next {
            TransitionOutcome::Success(raw) => { self.raw = raw; },
            _ => { assert(false); },
        }
        let out = $outcome(previous);
        self.pending = match out {
            ReleaseOutcome::BecameIdle => true,
            ReleaseOutcome::StillActive => false,
        };
        out
    }

    fn notify(&mut self)
        requires old(self).held, old(self).accessible, old(self).pending,
        ensures final(self).held, final(self).accessible, !final(self).pending,
            final(self).raw == old(self).raw,
    { self.pending = false; }

    fn unlock(&mut self, _guard: ())
        requires old(self).held, old(self).accessible, !old(self).pending,
        ensures !final(self).held, !final(self).accessible, !final(self).pending,
            final(self).raw == old(self).raw,
    {
        self.held = false;
        // No backend operation is legal after the reclamation boundary.
        self.accessible = false;
    }
}

/// Moving release before lock, notification after unlock, or omitting notify
/// makes this shared-body proof fail at the backend precondition.
pub fn release_tail_refines(model: &mut ReleaseModel) -> (out: ReleaseOutcome)
    requires old(model).accessible, !old(model).held, !old(model).pending,
        old(model).raw & $mask > 0,
    ensures !final(model).held, !final(model).accessible, !final(model).pending,
        $release_spec(old(model).raw) == TransitionOutcome::Success(final(model).raw),
{
    release_tail!(guard, outcome;
        model.lock(), model.release(), model.notify(), model.unlock(guard))
}

pub enum Ordering { AcqRel, Acquire, Relaxed }

/// An executable implementation of the primitive fetch_or contract. The native
/// atomic history is a TCB boundary; registration itself must use this operation
/// with AcqRel and return the pre-RMW active count.
pub struct RegistrationAtomic { pub raw: $word }
impl RegistrationAtomic {
    fn fetch_or(&mut self, bits: $word, ordering: Ordering) -> (previous: $word)
        requires ordering == Ordering::AcqRel,
        ensures previous == old(self).raw,
            final(self).raw == old(self).raw | bits,
    {
        let previous = self.raw;
        self.raw = previous | bits;
        previous
    }
}


/// `sample` may be stale: another admission can win after the load. CAS
/// compares against the actual current raw state rather than assuming success.
pub struct IdleSealAtomic { pub raw: $word, pub sample: $word }
impl IdleSealAtomic {
    fn load(&self, ordering: Ordering) -> (sample: $word)
        requires ordering == Ordering::Acquire,
        ensures sample == self.sample,
    { self.sample }

    fn compare_exchange(&mut self, expected: $word, next: $word,
        success: Ordering, failure: Ordering) -> (result: Result<$word, $word>)
        requires success == Ordering::AcqRel, failure == Ordering::Acquire,
        ensures final(self).sample == old(self).sample,
            result.is_ok() == (expected == old(self).raw),
            final(self).raw == (if expected == old(self).raw { next } else { old(self).raw }),
    {
        let previous = self.raw;
        if previous == expected { self.raw = next; Ok(previous) }
        else { Err(previous) }
    }
}

pub fn shared_idle_seal(atomic: &mut IdleSealAtomic) -> (success: bool)
    ensures final(atomic).sample == old(atomic).sample,
        success == (old(atomic).sample == old(atomic).raw
            && old(atomic).raw & ($sealed | $mask) == 0),
        final(atomic).raw == (if success { old(atomic).raw | $sealed } else { old(atomic).raw }),
        success ==> (final(atomic).raw & $sealed != 0 && final(atomic).raw & $mask == 0),
        final(atomic).raw & $waiting == old(atomic).raw & $waiting,
{
    let previous = atomic.raw;
    let success = super::super::transitions::try_idle_seal!(atomic, $sealed, $mask);
    assert((previous | $sealed) & $waiting == previous & $waiting) by(bit_vector);
    assert((previous | $sealed) & $mask == previous & $mask) by(bit_vector);
    assert((previous | $sealed) & $sealed != 0) by(bit_vector);
    assert((previous & ($sealed | $mask) == 0) ==> (previous & $mask == 0)) by(bit_vector);
    success
}

pub fn shared_undo_idle_seal(raw: $word) -> (next: Option<$word>)
    ensures next.is_some() == (raw & $sealed != 0 && raw & $mask == 0),
        next.is_some() ==> next.unwrap() == raw & !$sealed,
        next.is_some() ==> (next.unwrap() & $sealed == 0 && next.unwrap() & $mask == 0),
        next.is_some() ==> next.unwrap() & $waiting == raw & $waiting,
{
    let next = super::super::transitions::undo_idle_seal_logic!(raw, $sealed, $mask);
    assert((raw & !$sealed) & $sealed == 0) by(bit_vector);
    assert((raw & !$sealed) & $mask == raw & $mask) by(bit_vector);
    assert((raw & !$sealed) & $waiting == raw & $waiting) by(bit_vector);
    next
}


fn seal_stripe(raw: &mut Vec<$word>, index: usize, sample: $word) -> (success: bool)
    requires index < old(raw).len(),
    ensures final(raw).len() == old(raw).len(),
        final(raw)@ == old(raw)@.update(index as int,
            if success { old(raw)[index as int] | $sealed } else { old(raw)[index as int] }),
        success ==> old(raw)[index as int] & ($sealed | $mask) == 0,
        success ==> final(raw)[index as int] & $sealed != 0,
        success ==> final(raw)[index as int] & $mask == 0,
        success ==> final(raw)[index as int] & !$sealed == old(raw)[index as int],
        final(raw)[index as int] & $waiting == old(raw)[index as int] & $waiting,
{
    let previous = raw[index];
    let mut atomic = IdleSealAtomic { raw: raw[index], sample };
    let success = shared_idle_seal(&mut atomic);
    assert((previous & $sealed == 0) ==> ((previous | $sealed) & !$sealed == previous)) by(bit_vector);
    assert((previous & ($sealed | $mask) == 0) ==> (previous & $sealed == 0)) by(bit_vector);
    raw.set(index, atomic.raw);
    success
}

fn undo_stripe(raw: &mut Vec<$word>, index: usize)
    requires index < old(raw).len(),
        old(raw)[index as int] & $sealed != 0, old(raw)[index as int] & $mask == 0,
    ensures final(raw)@ == old(raw)@.update(index as int, old(raw)[index as int] & !$sealed),
{
    let next = shared_undo_idle_seal(raw[index]);
    raw.set(index, next.unwrap());
}

/// Exact rollback driver with independently stale load samples for each CAS.
/// Interference beyond those CAS races and registration is composed separately.
pub fn shared_seal_stripes(raw: &mut Vec<$word>, samples: &Vec<$word>) -> (success: bool)
    requires old(raw).len() == samples.len(),
        forall|i: int| #![auto] 0 <= i < old(raw).len() ==> old(raw)[i] & $sealed == 0,
    ensures final(raw).len() == old(raw).len(),
        !success ==> final(raw)@ == old(raw)@,
        success ==> (forall|i: int| #![auto] 0 <= i < final(raw).len() ==>
            final(raw)[i] & $sealed != 0 && final(raw)[i] & $mask == 0),
        forall|i: int| #![auto] 0 <= i < final(raw).len() ==>
            final(raw)[i] & $waiting == old(raw)[i] & $waiting,
{
    super::super::protocol::seal_stripes!(index, rollback;
        raw.len(), seal_stripe(raw, index, samples[index]), undo_stripe(raw, rollback);
        [invariant
            index <= raw.len(), raw.len() == old(raw).len(), raw.len() == samples.len(),
            forall|i: int| #![auto] 0 <= i < raw.len() ==> old(raw)[i] & $sealed == 0,
            forall|i: int| #![auto] 0 <= i < index ==>
                raw[i] == old(raw)[i] | $sealed && raw[i] & $sealed != 0
                && raw[i] & $mask == 0 && raw[i] & !$sealed == old(raw)[i]
                && raw[i] & $waiting == old(raw)[i] & $waiting,
            forall|i: int| #![auto] index <= i < raw.len() ==> raw[i] == old(raw)[i],
         decreases raw.len() - index,];
        [invariant
            rollback <= index, index < raw.len(), raw.len() == old(raw).len(),
            forall|i: int| #![auto] 0 <= i < raw.len() ==> old(raw)[i] & $sealed == 0,
            forall|i: int| #![auto] 0 <= i < rollback ==> raw[i] == old(raw)[i],
            forall|i: int| #![auto] rollback <= i < index ==>
                raw[i] == old(raw)[i] | $sealed && raw[i] & $sealed != 0
                && raw[i] & $mask == 0 && raw[i] & !$sealed == old(raw)[i]
                && raw[i] & $waiting == old(raw)[i] & $waiting,
            forall|i: int| #![auto] index <= i < raw.len() ==> raw[i] == old(raw)[i],
         decreases index - rollback,]
    )
}

pub struct WaitModel {
    pub raw: $word,
    pub held: bool,
    pub registered: bool,
    pub samples: Vec<$word>,
    pub cursor: usize,
}

impl WaitModel {
    fn lock(&mut self)
        requires !old(self).held,
        ensures final(self).held, final(self).raw == old(self).raw,
            final(self).registered == old(self).registered,
            final(self).samples == old(self).samples, final(self).cursor == old(self).cursor,
    { self.held = true; }

    /// RMW linearization observation under the notification mutex.
    fn register(&mut self) -> (active: $word)
        requires old(self).held,
        ensures final(self).held, final(self).registered,
            final(self).raw == old(self).raw | $waiting,
            final(self).raw & $waiting != 0,
            active == final(self).raw & $mask,
            final(self).samples == old(self).samples, final(self).cursor == old(self).cursor,
    {
        let previous = self.raw;
        let mut atomic = RegistrationAtomic { raw: previous };
        let active = super::super::transitions::mark_waiting!(atomic, $waiting, $mask);
        self.raw = atomic.raw;
        self.registered = true;
        assert((previous | $waiting) & $mask == previous & $mask) by(bit_vector);
        assert((previous | $waiting) & $waiting != 0) by(bit_vector);
        active
    }

    fn wait(&mut self, _guard: ())
        requires old(self).held, old(self).registered,
            old(self).raw & $mask != 0,
            old(self).cursor <= old(self).samples.len(),
        ensures final(self).held, !final(self).registered,
            final(self).cursor <= final(self).samples.len(),
    {
        // Atomic unlock/park/relock is a TCB operation. All interference
        // while unlocked is represented by an arbitrary next raw state.
        if self.cursor < self.samples.len() {
            self.raw = self.samples[self.cursor];
            self.cursor = self.cursor + 1;
        }
        self.registered = false;
    }

    fn unlock(&mut self, _guard: ())
        requires old(self).held, old(self).registered,
            old(self).raw & $mask == 0,
        ensures !final(self).held, final(self).registered, final(self).raw == old(self).raw,
    { self.held = false; }
}

/// A wake never certifies idle: the shared loop must register and observe again.
#[verifier::exec_allows_no_decreases_clause]
pub fn wait_loop_refines(model: &mut WaitModel)
    requires !old(model).held, old(model).cursor <= old(model).samples.len(),
    ensures !final(model).held, final(model).registered,
        final(model).raw & $mask == 0, final(model).raw & $waiting != 0,
{
    wait_loop!(guard, active;
        model.lock(), model.register(), model.wait(guard), model.unlock(guard);
        invariant
            model.held, model.registered,
            model.raw & $waiting != 0,
            model.cursor <= model.samples.len(),
            active == model.raw & $mask,
    );
}

fn register_stripe(raw: &mut Vec<$word>, index: usize) -> (count: $word)
    requires index < old(raw).len(),
    ensures final(raw).len() == old(raw).len(),
        final(raw)@ == old(raw)@.update(index as int, old(raw)[index as int] | $waiting),
        count == final(raw)[index as int] & $mask,
        (forall|i: int| #![auto] 0 <= i < index + 1 ==> final(raw)[i] & $mask == 0)
            == ((forall|i: int| #![auto] 0 <= i < index ==> old(raw)[i] & $mask == 0) && count == 0),
{
    let ghost before = raw@;
    let previous = raw[index];
    let mut atomic = RegistrationAtomic { raw: previous };
    let count = super::super::transitions::mark_waiting!(atomic, $waiting, $mask);
    assert((previous | $waiting) & $mask == previous & $mask) by(bit_vector);
    raw.set(index, atomic.raw);
    proof {
        if (forall|i: int| #![auto] 0 <= i < index ==> before[i] & $mask == 0) && count == 0 {
            assert forall|i: int| #![auto] 0 <= i < index + 1 implies raw[i] & $mask == 0 by {
                if i < index { assert(before[i] & $mask == 0); }
                else { assert(i == index); }
            }
        }
        if forall|i: int| #![auto] 0 <= i < index + 1 ==> raw[i] & $mask == 0 {
            assert(raw[index as int] & $mask == 0);
            assert forall|i: int| #![auto] 0 <= i < index implies before[i] & $mask == 0 by {
                assert(raw[i] & $mask == 0);
            }
        }
    }
    count
}

/// The scan runs with the notification lock held. Each entry is an RMW
/// linearization sample. Cross-stripe stability additionally needs sealing and
/// exclusion of reopen, as proved separately for the counter transitions.
pub fn scan_stripes(raw: &mut Vec<$word>) -> (idle: bool)
    ensures final(raw).len() == old(raw).len(),
        forall|i: int| #![auto] 0 <= i < old(raw).len() ==>
            final(raw)[i] == old(raw)[i] | $waiting,
        idle == (forall|i: int| #![auto] 0 <= i < final(raw).len() ==>
            final(raw)[i] & $mask == 0),
{
    super::super::protocol::observe_stripes!(index, idle;
        raw.len(), register_stripe(raw, index);
        invariant
            index <= raw.len(), raw.len() == old(raw).len(),
            forall|i: int| #![auto] 0 <= i < index ==> raw[i] == old(raw)[i] | $waiting,
            forall|i: int| #![auto] index <= i < raw.len() ==> raw[i] == old(raw)[i],
            idle == (forall|i: int| #![auto] 0 <= i < index ==> raw[i] & $mask == 0),
        decreases raw.len() - index,
    )
}


/// Rely relation while the transition owner excludes reopen and idle-seal undo.
/// Includes rejected/failed RMWs, loads, waiter registration, repeat sealing,
/// and successful reader RMWs from the actual shared transition definitions.
pub open spec fn reader_interference(before: $word, after: $word) -> bool {
    ||| after == before
    ||| after == before | $waiting
    ||| after == before | $sealed
    ||| $acquire_spec(before) == TransitionOutcome::Success(after)
    ||| $release_spec(before) == TransitionOutcome::Success(after)
}

pub proof fn sealed_step_is_nonincreasing(before: $word, after: $word)
    requires before & $sealed != 0, reader_interference(before, after),
    ensures after & $sealed != 0, after & $mask <= before & $mask,
{
    assert((before | $waiting) & $sealed == before & $sealed) by(bit_vector);
    assert((before | $waiting) & $mask == before & $mask) by(bit_vector);
    assert((before | $sealed) & $sealed != 0) by(bit_vector);
    assert((before | $sealed) & $mask == before & $mask) by(bit_vector);
    if $release_spec(before) == TransitionOutcome::Success(after) {
        assert(before & $mask != 0 && after == before.wrapping_sub(1));
        assert((before & $mask != 0 && after == before.wrapping_sub(1)) ==>
            ((after & $sealed) == (before & $sealed)
            && (after & $mask) < (before & $mask))) by(bit_vector);
    }
}

pub open spec fn reader_history(history: Seq<$word>) -> bool {
    history.len() > 0 && (forall|i: int| #![trigger history[i]] 0 <= i < history.len() - 1 ==>
        reader_interference(history[i], history[i + 1]))
}

pub proof fn sealed_history_is_nonincreasing(history: Seq<$word>)
    requires reader_history(history), history[0] & $sealed != 0,
    ensures history.last() & $sealed != 0,
        history.last() & $mask <= history[0] & $mask,
    decreases history.len(),
{
    if history.len() > 1 {
        let prefix = history.subrange(0, history.len() as int - 1);
        sealed_history_is_nonincreasing(prefix);
        sealed_step_is_nonincreasing(history[history.len() as int - 2], history.last());
    }
}

/// One stripe's history starts at its own observation and ends at a later
/// reclamation point. Apply independently to each stripe; no common initial
/// snapshot and no finite bound on the number of reader operations is needed.
pub proof fn observed_idle_history_has_no_permits(history: Seq<$word>, permits: nat)
    requires reader_history(history), history[0] & $sealed != 0,
        history[0] & $mask == 0,
        super::super::drain_gate_inv(decode(history.last(), permits)),
    ensures history.last() & $sealed != 0, history.last() & $mask == 0, permits == 0,
{
    sealed_history_is_nonincreasing(history);
}

pub proof fn scanned_idle_has_no_permits(raw: Seq<$word>, permits: Seq<nat>)
    requires raw.len() == permits.len(),
        forall|i: int| #![auto] 0 <= i < raw.len() ==>
            super::super::drain_gate_inv(decode(raw[i], permits[i])),
        forall|i: int| #![auto] 0 <= i < raw.len() ==> raw[i] & $mask == 0,
    ensures forall|i: int| #![auto] 0 <= i < permits.len() ==> permits[i] == 0,
{}

} // verus!
}
    };
}

verify_width!(word32, u32, ACTIVE_COUNT_MASK_32, WAITING_BIT_32,
    release_step_32, release_spec_32, release_outcome_step_32,
    release_without_notification_step_32, SEALED_BIT_32, acquire_spec_32, reopen_spec_32);
verify_width!(word64, u64, ACTIVE_COUNT_MASK_64, WAITING_BIT_64,
    release_step_64, release_spec_64, release_outcome_step_64,
    release_without_notification_step_64, SEALED_BIT_64, acquire_spec_64, reopen_spec_64);
