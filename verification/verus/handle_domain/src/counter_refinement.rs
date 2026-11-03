//! Finite-width counter bridges at completed enqueue/detach boundaries.
//! Concurrent native intermediate states still require their own representation.
use vstd::prelude::*;
use super::*;
use super::counters::CountStep;
macro_rules! width {
    ($module:ident, $word:ty) => {
    pub mod $module {
    use super::*;
    use super::super::counters::$module as kernel;
    verus! {
    pub(crate) fn enqueue(debt: $word, queued: $word, Ghost(model): Ghost<HandleDomainState>)
        -> (result: CountStep<($word, $word)>)
        requires handle_domain_inv(model), model.active_bindings > 0,
            debt as nat == model.debt, queued as nat == model.pending_0 + model.pending_1,
        ensures match result {
            CountStep::Success((next_debt, next_queued)) => {
                let next = step_enqueue_reclaim(model).unwrap();
                next_debt as nat == next.debt && next_queued as nat == next.pending_0 + next.pending_1
                && handle_domain_inv(next)
            },
            CountStep::FailStop => model.debt + 1 > <$word>::MAX as nat,
        },
    {
        match kernel::add(debt, 1) {
            CountStep::FailStop => CountStep::FailStop,
            CountStep::Success(next_debt) => {
                match kernel::add(queued, 1) {
                    CountStep::FailStop => { assert(false); CountStep::FailStop },
                    CountStep::Success(next_queued) => {
                        proof { hd_step_enqueue_reclaim_preserves_inv(model, step_enqueue_reclaim(model).unwrap()); }
                        CountStep::Success((next_debt, next_queued))
                    },
                }
            },
        }
    }

    pub(crate) fn detach(queued: $word, amount: $word, Ghost(model): Ghost<HandleDomainState>, Ghost(generation): Ghost<nat>)
        -> (result: $word)
        requires handle_domain_inv(model), step_detach_generation(model, generation).is_some(),
            queued as nat == model.pending_0 + model.pending_1, amount as nat == get_pending(model, generation),
        ensures result as nat == step_detach_generation(model, generation).unwrap().pending_0
            + step_detach_generation(model, generation).unwrap().pending_1,
            step_detach_generation(model, generation).unwrap().debt == model.debt,
            handle_domain_inv(step_detach_generation(model, generation).unwrap()),
    {
        let result = kernel::subtract(queued, amount);
        proof { hd_step_detach_generation_preserves_inv(model, generation, step_detach_generation(model, generation).unwrap()); }
        match result {
            CountStep::Success(next) => next,
            CountStep::FailStop => { assert(false); 0 },
        }
    }
    }
    }
    };
}
width!(word32, u32);
width!(word64, u64);
