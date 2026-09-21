//! Array-position mapping between the shared stripe scan and linear permits.
use vstd::prelude::*;
use super::permits::admission;
use super::transitions::*;
macro_rules! width {
    ($module:ident, $word:ty, $mask:ident, $waiting:ident, $sealed:ident) => {
    pub mod $module {
    use super::*;
    verus! {
    pub open spec fn drained_histories(histories: Seq<Seq<$word>>) -> bool {
        forall|i: int| 0 <= i < histories.len() ==> (
            super::super::refinement::$module::reader_history(#[trigger] histories[i])
            && histories[i][0] & $sealed != 0 && histories[i][0] & $mask == 0)
    }
    pub open spec fn final_states(histories: Seq<Seq<$word>>) -> Seq<$word> {
        histories.map(|i: int, history: Seq<$word>| history.last())
    }

    pub tracked struct StripeLedgers {
        pub gates: Seq<admission::Instance>,
        pub counts: Seq<admission::active>,
    }
    impl StripeLedgers {
        pub open spec fn matches(&self, raw: Seq<$word>) -> bool {
            &&& self.gates.len() == raw.len()
            &&& self.counts.len() == raw.len()
            &&& forall|i: int| 0 <= i < raw.len() ==> (
                (#[trigger] self.counts[i]).instance_id() == self.gates[i].id()
                && self.counts[i].value() == (raw[i] & $mask) as nat)
            &&& forall|i: int, j: int| 0 <= i < j < raw.len() ==>
                (#[trigger] self.gates[i]).id() != (#[trigger] self.gates[j]).id()
        }
    }

    /// Exact identity set of every stripe position, without collapsing the
    /// generation to one representative admission instance.
    pub open spec fn domain(ledgers: &StripeLedgers) -> Set<vstd::tokens::InstanceId> {
        ledgers.gates.map(|index: int, gate: admission::Instance| gate.id()).to_set()
    }
    pub proof fn member_at(tracked ledgers: &StripeLedgers, index: int)
        requires 0 <= index < ledgers.gates.len(),
        ensures domain(ledgers).contains(ledgers.gates[index].id()),
    { assert(ledgers.gates.map(|i: int, gate: admission::Instance| gate.id())[index] == ledgers.gates[index].id()); }
    /// The same all-stripe registration/scan used in production preserves
    /// the count-to-instance mapping and identifies every zero ledger.
    pub fn scan_with_ledgers(raw: &mut Vec<$word>,
        Tracked(ledgers): Tracked<&StripeLedgers>) -> (idle: bool)
        requires ledgers.matches(old(raw)@),
        ensures ledgers.matches(final(raw)@),
            idle == (forall|i: int| 0 <= i < ledgers.counts.len() ==>
                (#[trigger] ledgers.counts[i]).value() == 0),
    {
        let ghost before = raw@;
        let idle = super::super::refinement::$module::scan_stripes(raw);
        proof {
            assert forall|i: int| #![auto] 0 <= i < raw.len() implies
                raw[i] & $mask == before[i] & $mask by {
                let old_word = before[i];
                assert((old_word | $waiting) & $mask == old_word & $mask) by(bit_vector);
            }
            assert(ledgers.matches(raw@));
            if idle {
                assert forall|i: int| 0 <= i < ledgers.counts.len() implies
                    (#[trigger] ledgers.counts[i]).value() == 0 by {
                    assert(raw[i] & $mask == 0);
                }
            } else if forall|i: int| 0 <= i < ledgers.counts.len() ==>
                (#[trigger] ledgers.counts[i]).value() == 0 {
                assert forall|i: int| #![auto] 0 <= i < raw.len() implies raw[i] & $mask == 0 by {
                    assert(ledgers.counts[i].value() == 0);
                }
            }
        }
        idle
    }

    /// Stripe observations may occur at different times. Every intervening
    /// step follows the shared counter rely relation while reopen is excluded.
    /// Therefore sampled sealed zeros are still zero at drain completion.
    pub proof fn histories_exclude_permit(histories: Seq<Seq<$word>>,
        tracked ledgers: &StripeLedgers, tracked permit: &admission::permits, index: int)
        requires drained_histories(histories), ledgers.matches(final_states(histories)),
            0 <= index < histories.len(), permit.instance_id() == ledgers.gates[index].id(),
        ensures false,
    {
        super::super::refinement::$module::sealed_history_is_nonincreasing(histories[index]);
        assert(histories[index].last() & $mask == 0);
        let tracked gate = ledgers.gates.tracked_borrow(index);
        let tracked count = ledgers.counts.tracked_borrow(index);
        gate.positive(count, permit);
    }

    /// A permit cannot hide in an unvisited stripe: all positions are covered.
    pub proof fn all_idle_excludes_permit(tracked ledgers: &StripeLedgers,
        tracked permit: &admission::permits, index: int)
        requires 0 <= index < ledgers.counts.len(),
            ledgers.gates.len() == ledgers.counts.len(),
            ledgers.counts[index].instance_id() == ledgers.gates[index].id(),
            permit.instance_id() == ledgers.gates[index].id(),
            forall|i: int| 0 <= i < ledgers.counts.len() ==>
                (#[trigger] ledgers.counts[i]).value() == 0,
        ensures false,
    {
        let tracked gate = ledgers.gates.tracked_borrow(index);
        let tracked count = ledgers.counts.tracked_borrow(index);
        gate.positive(count, permit);
    }
    }
    }
    };
}
width!(word32, u32, ACTIVE_COUNT_MASK_32, WAITING_BIT_32, SEALED_BIT_32);
width!(word64, u64, ACTIVE_COUNT_MASK_64, WAITING_BIT_64, SEALED_BIT_64);
