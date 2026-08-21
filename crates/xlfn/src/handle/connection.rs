#[cfg(any(target_os = "windows", test))]
use super::*;
use super::{HandleId, ObjectId, PublishedTopic};
use crate::generation::ServerGeneration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FormulaBinding {
    pub(crate) id: HandleId,
    pub(crate) object_id: ObjectId,
}

pub(crate) struct Topic {
    /// The immutable publication owns the formula binding and wire identities.
    pub(crate) publication: triomphe::Arc<PublishedTopic>,
    #[cfg(any(target_os = "windows", test))]
    pub(crate) server_generation: Option<ServerGeneration>,
    pub(crate) excel_topic: Option<HandleTopicOwner>,
    #[cfg(any(target_os = "windows", test))]
    pub(crate) excel_topic_committed: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct HandleTopicOwner {
    pub(crate) server_generation: ServerGeneration,
    pub(crate) topic_id: i32,
}

#[cfg(any(target_os = "windows", test))]
/// Provisional Excel topic assignment borrowing the handle runtime that
/// created it. The borrow keeps commit and rollback on the same live runtime
/// without a temporary `Weak` upgrade.
pub(crate) struct HandleConnection<'runtime> {
    pub(crate) runtime: &'runtime HandleRuntime,
    pub(crate) owner: HandleTopicOwner,
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
