use super::PublishedTopic;
#[cfg(any(target_os = "windows", test))]
use super::*;
use std::sync::Arc;
#[cfg(any(target_os = "windows", test))]
use std::sync::Weak;

pub(crate) struct Topic {
    pub(crate) token: String,
    pub(crate) rtd_key: Arc<str>,
    pub(crate) publication: Arc<PublishedTopic>,
    #[cfg(any(target_os = "windows", test))]
    pub(crate) server_generation: Option<u64>,
    pub(crate) excel_topic: Option<HandleTopicOwner>,
    #[cfg(any(target_os = "windows", test))]
    pub(crate) excel_topic_committed: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct HandleTopicOwner {
    pub(crate) server_generation: u64,
    pub(crate) topic_id: i32,
}

#[cfg(any(target_os = "windows", test))]
pub(crate) struct HandleConnection {
    pub(crate) runtime: Weak<HandleRuntime>,
    pub(crate) owner: HandleTopicOwner,
    pub(crate) key: HandleTopicKey,
    pub(crate) token: String,
    pub(crate) created: bool,
    pub(crate) finished: bool,
}

#[cfg(any(target_os = "windows", test))]
impl HandleConnection {
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub(crate) fn commit(mut self) -> XllResult<()> {
        if self.finished {
            return Ok(());
        }
        if self.created {
            let runtime = self.runtime.upgrade().ok_or(XllError::Closing)?;
            runtime.commit_connection(self.owner, self.key)?;
        }
        self.finished = true;
        Ok(())
    }

    pub(crate) fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.created
            && let Some(runtime) = self.runtime.upgrade()
        {
            runtime.rollback_connection(self.owner, self.key);
        }
    }
}

#[cfg(any(target_os = "windows", test))]
impl Drop for HandleConnection {
    fn drop(&mut self) {
        self.finish();
    }
}
