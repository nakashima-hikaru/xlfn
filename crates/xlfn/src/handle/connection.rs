#[cfg(any(target_os = "windows", test))]
use super::FormulaHandleService;
use super::FormulaLifetimeGeneration;
#[cfg(any(target_os = "windows", test))]
use super::HandleTopicKey;
use super::PublishedTopic;
#[cfg(any(target_os = "windows", test))]
use crate::XllResult;
use xlfn_kernel::published_owner::PublishedOwner;

pub(crate) struct Topic {
    /// Heap-allocated publication. Non-owning PublishedTopicPtr references this.
    pub(crate) publication: PublishedOwner<PublishedTopic>,
    #[cfg(any(target_os = "windows", test))]
    pub(crate) lifetime_generation: Option<FormulaLifetimeGeneration>,
    pub(crate) observer: Option<FormulaObserverId>,
    #[cfg(any(target_os = "windows", test))]
    pub(crate) observer_committed: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FormulaObserverId {
    pub(crate) generation: FormulaLifetimeGeneration,
    pub(crate) topic_id: i32,
}

#[cfg(any(target_os = "windows", test))]
/// Provisional Excel topic assignment borrowing the handle runtime that
/// created it. The borrow keeps commit and rollback on the same live runtime
/// without a temporary `Weak` upgrade.
pub(crate) struct HandleConnection<'runtime> {
    runtime: &'runtime FormulaHandleService,
    token: String,
    disposition: Option<ConnectionDisposition>,
}

#[cfg(any(target_os = "windows", test))]
enum ConnectionDisposition {
    Existing,
    NewlyReserved {
        owner: FormulaObserverId,
        key: HandleTopicKey,
    },
}

#[cfg(any(target_os = "windows", test))]
impl HandleConnection<'_> {
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub(crate) fn commit(mut self) -> XllResult<()> {
        if let Some(ConnectionDisposition::NewlyReserved { owner, key }) = self.disposition
            && let Err(error) = self.runtime.commit_connection(owner, key)
        {
            self.rollback();
            return Err(error);
        }
        self.disposition = None;
        Ok(())
    }

    pub(crate) fn rollback(mut self) {
        self.rollback_inner();
    }

    fn rollback_inner(&mut self) {
        if let Some(ConnectionDisposition::NewlyReserved { owner, key }) = self.disposition.take() {
            self.runtime.rollback_connection(owner, key);
        }
    }
}

#[cfg(any(target_os = "windows", test))]
impl Drop for HandleConnection<'_> {
    fn drop(&mut self) {
        self.rollback_inner();
    }
}

#[cfg(any(target_os = "windows", test))]
impl FormulaHandleService {
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn connect_transaction(
        &self,
        lifetime_generation: FormulaLifetimeGeneration,
        topic_id: i32,
        lifetime_key: &str,
    ) -> XllResult<HandleConnection<'_>> {
        let owner = FormulaObserverId {
            generation: lifetime_generation,
            topic_id,
        };
        let (key, token, created) =
            self.connect_inner(lifetime_generation, topic_id, lifetime_key)?;
        Ok(HandleConnection {
            runtime: self,
            token,
            disposition: Some(if created {
                ConnectionDisposition::NewlyReserved { owner, key }
            } else {
                ConnectionDisposition::Existing
            }),
        })
    }
}
