//! Feature-gated façade for the executable handle refinement trace.
//!
//! Operational handle code talks to this façade instead of depending on the
//! trace machine's storage type.  Production builds retain no trace state;
//! test and checker builds delegate to `refinement.rs`.

use super::HandleTopicKey;
#[cfg(any(test, all(target_os = "windows", feature = "handle-refinement-trace")))]
use super::HandleTopicOwner;
#[cfg(any(test, all(target_os = "windows", feature = "handle-refinement-trace")))]
use crate::generation::ServerGeneration;

#[cfg(any(test, feature = "handle-refinement-trace"))]
use super::refinement::{HandleRefinementTrace, Linearization, PrepareGuard, TokenWire};

pub(crate) struct HandleRefinementHooks {
    #[cfg(any(test, feature = "handle-refinement-trace"))]
    trace: HandleRefinementTrace,
}

impl HandleRefinementHooks {
    pub(crate) fn new(session: u64) -> Self {
        Self {
            #[cfg(any(test, feature = "handle-refinement-trace"))]
            trace: HandleRefinementTrace::new(session),
        }
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn prepare_guard(&self) -> PrepareGuard<'_> {
        self.trace.prepare_guard()
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn linearize(&self) -> Linearization<'_> {
        self.trace.linearize()
    }

    #[cfg(test)]
    pub(crate) fn set_before_seal_hook(
        &self,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) {
        self.trace.set_before_seal_hook(entered, release);
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn begin_prepare(&self) {
        self.trace.begin_prepare();
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn allocate_initializer_id(&self) -> u64 {
        self.trace.allocate_initializer_id()
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn begin_initializer(&self, key: &HandleTopicKey, runtime_id: u64) {
        self.trace.begin_initializer(key, runtime_id);
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn insert_pending_fresh(&self, key: &HandleTopicKey, runtime_id: u64) {
        self.trace.insert_pending_fresh(key, runtime_id);
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn insert_pending_reuse(
        &self,
        key: &HandleTopicKey,
        runtime_id: u64,
        slot: u64,
        generation: u64,
    ) {
        self.trace
            .insert_pending_reuse(key, runtime_id, slot, generation);
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn publish_and_install(
        &self,
        key: &HandleTopicKey,
        runtime_id: u64,
        token: TokenWire,
        rtd_key: &str,
    ) {
        self.trace
            .publish_and_install(key, runtime_id, token, rtd_key);
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn rollback_pending(
        &self,
        key: &HandleTopicKey,
        runtime_id: u64,
        reusable: bool,
        token: TokenWire,
    ) {
        self.trace
            .rollback_pending(key, runtime_id, reusable, token);
    }

    #[cfg(any(test, all(target_os = "windows", feature = "handle-refinement-trace")))]
    pub(crate) fn claim_server(&self, key: &HandleTopicKey, generation: ServerGeneration) {
        self.trace.claim_server(key, generation);
    }

    #[cfg(any(test, all(target_os = "windows", feature = "handle-refinement-trace")))]
    pub(crate) fn begin_connection(&self, key: &HandleTopicKey, owner: HandleTopicOwner) {
        self.trace.begin_connection(key, owner);
    }

    #[cfg(any(test, all(target_os = "windows", feature = "handle-refinement-trace")))]
    pub(crate) fn reuse_committed_connection(&self, key: &HandleTopicKey, owner: HandleTopicOwner) {
        self.trace.reuse_committed_connection(key, owner);
    }

    #[cfg(any(test, all(target_os = "windows", feature = "handle-refinement-trace")))]
    pub(crate) fn commit_connection(&self, key: &HandleTopicKey, owner: HandleTopicOwner) {
        self.trace.commit_connection(key, owner);
    }

    #[cfg(any(test, all(target_os = "windows", feature = "handle-refinement-trace")))]
    pub(crate) fn rollback_connection(&self, key: &HandleTopicKey, owner: HandleTopicOwner) {
        self.trace.rollback_connection(key, owner);
    }

    #[cfg(any(test, all(target_os = "windows", feature = "handle-refinement-trace")))]
    pub(crate) fn drain_pending(&self, token: TokenWire, runtime_id: u64, reusable: bool) {
        self.trace.drain_pending(token, runtime_id, reusable);
    }

    #[cfg(any(test, all(target_os = "windows", feature = "handle-refinement-trace")))]
    pub(crate) fn drain_published(&self, token: TokenWire, reusable: bool) {
        self.trace.drain_published(token, reusable);
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn close_registry(&self) {
        self.trace.close_registry();
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn finish_close(&self) {
        self.trace.finish_close();
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) fn mark_returned_success(&self) {
        self.trace.mark_returned_success();
    }

    #[cfg(test)]
    pub(crate) fn trace_json(&self) -> String {
        self.trace.trace_json()
    }
}
