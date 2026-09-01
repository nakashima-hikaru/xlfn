#[cfg(any(target_os = "windows", test))]
use super::FormulaHandleService;
use super::FormulaLifetimeGeneration;
#[cfg(any(target_os = "windows", test))]
use super::HandleTopicKey;
use super::PublishedTopicPtr;
#[cfg(any(target_os = "windows", test))]
use crate::XllResult;

pub(crate) struct Topic {
    /// Non-owning identity into the topic table's publication arena.
    pub(crate) publication: PublishedTopicPtr,
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
    pub(crate) runtime: &'runtime FormulaHandleService,
    pub(crate) owner: FormulaObserverId,
    pub(crate) key: HandleTopicKey,
    pub(crate) token: String,
    pub(crate) created: bool,
    pub(crate) finished: bool,
}

#[cfg(any(target_os = "windows", test))]
impl HandleConnection<'_> {
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub(crate) fn commit(mut self) -> XllResult<()> {
        if self.finished {
            return Ok(());
        }
        if self.created {
            self.runtime.commit_connection(self.owner, self.key)?;
        }
        self.finished = true;
        Ok(())
    }

    pub(crate) fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.created {
            self.runtime.rollback_connection(self.owner, self.key);
        }
    }
}

#[cfg(any(target_os = "windows", test))]
impl Drop for HandleConnection<'_> {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(target_os = "windows")]
impl super::lifetime::FormulaLifetimeConnection for HandleConnection<'_> {
    fn token(&self) -> &str {
        self.token()
    }

    fn commit(self: Box<Self>) -> crate::XllResult<()> {
        HandleConnection::commit(*self)
    }
}
