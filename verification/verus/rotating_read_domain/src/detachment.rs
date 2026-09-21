//! Certificate-authorized queue detachment using production-shared control flow.
use vstd::prelude::*;
use super::registration::Registration;
macro_rules! width {
    ($width:ident) => {
    pub mod $width {
    use super::*;
    use super::super::identity::$width::{Drained, Closed};
    verus! {
    pub fn detach_terminal<P>(queue: &mut Registration<P>, certificate: &Closed<'_>)
        -> (batches: Option<[Vec<P>; 2]>)
        requires certificate.inv(), old(queue).held.is_none(), !old(queue).both_held,
        ensures final(queue).held.is_none(), !final(queue).both_held,
            final(queue).owner == old(queue).owner,
            batches.is_some() == (old(queue).owner.addr() == certificate.state().domain.addr()),
            match batches {
                Some(pair) => pair[0]@ == old(queue).zero@ && pair[1]@ == old(queue).one@
                    && final(queue).zero@ == Seq::<P>::empty() && final(queue).one@ == Seq::<P>::empty(),
                None => final(queue).zero@ == old(queue).zero@ && final(queue).one@ == old(queue).one@,
            },
    {
        super::super::protocol::take_authorized_queues!(indices, first, second;
            certificate.indices_for(queue.owner), queue.lock_terminal(indices[0]),
            queue.lock_terminal(indices[1]), queue.take_pair(first, second))
    }

    pub fn detach<P>(queue: &mut Registration<P>, certificate: &Drained<'_>) -> (entries: Option<Vec<P>>)
        requires certificate.inv(), old(queue).held.is_none() && !old(queue).both_held,
        ensures final(queue).held.is_none(), !final(queue).both_held, final(queue).owner == old(queue).owner,
            entries.is_some() == (old(queue).owner.addr() == certificate.state().domain.addr()),
            match entries {
                Some(batch) => batch@ == if certificate.index() == 1 { old(queue).one@ } else { old(queue).zero@ }
                    && final(queue).zero@ == if certificate.index() == 1 { old(queue).zero@ } else { Seq::empty() }
                    && final(queue).one@ == if certificate.index() == 1 { Seq::empty() } else { old(queue).one@ },
                None => final(queue).zero@ == old(queue).zero@ && final(queue).one@ == old(queue).one@,
            },
    {
        super::super::protocol::take_authorized_queue!(index, guard;
            certificate.index_for(queue.owner), queue.lock_drain(index), queue.take_locked(index, guard))
    }
    }
    }
    };
}
width!(word32);
width!(word64);
