//! Published topic allocations retain their payload through abandonment or
//! unwind. Only this adapter detaches their own reader-drained generation and
//! exposes ordinary destruction, after all kernel/table locks have ended.
use super::*;
use std::mem::ManuallyDrop;
use xlfn_kernel::published_owner::PublishedOwner;

#[cfg(test)]
mod tests;

pub(super) struct PublishedTopicOwner {
    owner: ManuallyDrop<PublishedOwner<PublishedTopic>>,
}

impl PublishedTopicOwner {
    pub(super) fn new(topic: PublishedTopic) -> Self {
        Self {
            owner: ManuallyDrop::new(PublishedOwner::new(topic)),
        }
    }

    fn reclaim(self) {
        drop(ManuallyDrop::into_inner(self.owner));
    }
}

impl AsRef<PublishedTopic> for PublishedTopicOwner {
    fn as_ref(&self) -> &PublishedTopic {
        self.owner.as_ref()
    }
}

impl Deref for PublishedTopicOwner {
    type Target = PublishedTopic;
    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

// The common single-topic disconnect remains inline and carries no new counts.
pub(super) type RetiredTopics = SmallVec<[PublishedTopicOwner; 4]>;

pub(in crate::handle) struct DrainedTopics {
    topics: RetiredTopics,
}

impl DrainedTopics {
    #[cfg(test)]
    fn empty() -> Self {
        Self {
            topics: SmallVec::new(),
        }
    }

    #[cfg(test)]
    pub(in crate::handle) fn len(&self) -> usize {
        self.topics.len()
    }
}

impl Drop for DrainedTopics {
    fn drop(&mut self) {
        for topic in self.topics.drain(..) {
            topic.reclaim();
        }
    }
}

/// Only matching initialization and provisional-owner validation can issue
/// this consuming publication authority. It borrows the actual locked topic;
/// neither a different pointer nor another map can be supplied at commit.
struct TopicPublication<'topic> {
    target: &'topic PublishedTopics,
    topic: &'topic mut Topic,
    key: HandleTopicKey,
}
impl TopicPublication<'_> {
    fn publish(self) {
        let pointer = PublishedTopicPtr::from_ref(self.topic.publication.as_ref());
        self.topic
            .publication
            .state
            .store(PublishedTopicState::Live as u8, Ordering::Release);
        self.target.entries.pin().insert(self.key, pointer);
    }
}

pub(super) fn commit_publication(
    table: &TopicTable,
    key: HandleTopicKey,
    generation: TopicGeneration,
    initialization: &InitializationPtr,
    publication: PublishedTopicPtr,
    on_linearized: impl FnOnce(),
) -> XllResult<()> {
    let mut state = table.state.write();
    if state.closed || state.generation != generation {
        return Err(XllError::Closing);
    }
    if state.initializing.get(&key) != Some(initialization) {
        return Err(XllError::StaleHandle);
    }
    let topic = state.by_key.get_mut(&key).ok_or(XllError::StaleHandle)?;
    if PublishedTopicPtr::from_ref(topic.publication.as_ref()) != publication {
        return Err(XllError::StaleHandle);
    }
    TopicPublication {
        target: &table.published,
        topic,
        key,
    }
    .publish();
    state.initializing.remove(&key);
    on_linearized();
    Ok(())
}

impl TopicTable {
    fn account_retired(&self, retired: RetiredTopics) -> DrainedTopics {
        xlfn_kernel::invariant::checked_atomic_sub_relaxed(&self.queued, retired.len());
        DrainedTopics { topics: retired }
    }

    #[cfg(test)]
    pub(in crate::handle) fn try_quiesce_and_drain(&self) -> DrainedTopics {
        if self.queued.load(Ordering::Relaxed) == 0 {
            return DrainedTopics::empty();
        }
        let Some(result) = self
            .read_domain
            .poll_quiesce()
            .map(|result| result.map(|batch| self.account_retired(batch.into_inner())))
        else {
            return DrainedTopics::empty();
        };
        result.unwrap_or_else(|_| DrainedTopics::empty())
    }

    pub(super) fn maintain_after_reader(&self) {
        // Acquire the seal's Release sequence after the permit's Release
        // RMW. If departure preceded seal, the writer observes the idle gate
        // and drains; otherwise this fence makes its queued work visible.
        // This is the same final-reader handoff as HandleReadDomain.
        std::sync::atomic::fence(Ordering::Acquire);
        if self.queued.load(Ordering::Relaxed) != 0 {
            self.maintain();
        }
    }

    pub(super) fn maintain(&self) {
        // One requester drives each poll. A departing reader that encounters
        // its transition/queue lock registers a further pass before returning,
        // so the active driver cannot discard the final-reader notification.
        let previous = self
            .maintenance_requests
            .try_update(Ordering::AcqRel, Ordering::Acquire, |requests| {
                requests.checked_add(1)
            })
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        if previous != 0 {
            return;
        }
        let mut consumed = 1;
        loop {
            while self.queued.load(Ordering::Relaxed) != 0 {
                let Some(Ok(retired)) = self
                    .read_domain
                    .poll_quiesce()
                    .map(|result| result.map(|batch| self.account_retired(batch.into_inner())))
                else {
                    break;
                };
                // Only framework-owned strings are destroyed here. The queue,
                // transition and topic-table guards have all ended.
                drop(retired);
            }
            let requested =
                xlfn_kernel::invariant::checked_atomic_sub(&self.maintenance_requests, consumed);
            if requested == consumed {
                return;
            }
            consumed = requested - consumed;
        }
    }

    pub(in crate::handle) fn seal_and_drain(&self) -> DrainedTopics {
        let closed = self.read_domain.seal_and_wait();
        let [first, second] = closed.take_queues();
        let mut first = self.account_retired(first.into_inner());
        let mut second = self.account_retired(second.into_inner());
        first.topics.append(&mut second.topics);
        first
    }
}

impl Drop for TopicTable {
    fn drop(&mut self) {
        // Exclusive table destruction withdraws remaining publications before
        // draining its domain. Unwind retains protected owners instead of
        // letting ordinary field drop bypass that grace period.
        let _initializations = self.close();
        drop(self.seal_and_drain());
    }
}
