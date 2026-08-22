//! Feature-gated façade for the executable handle refinement trace.
//!
//! Operational handle code talks to this façade instead of depending on the
//! trace machine's storage type.  Production builds retain no trace state;
//! test and checker builds delegate to `refinement.rs`.

use super::HandleTopicKey;
#[cfg(any(target_os = "windows", test))]
use super::HandleTopicOwner;
use super::refinement_wire::TokenWire;
#[cfg(any(target_os = "windows", test))]
use crate::generation::ServerGeneration;

#[cfg(any(test, feature = "unstable"))]
use super::refinement::HandleRefinementTrace;

pub(crate) struct HandleRefinementHooks {
    #[cfg(any(test, feature = "unstable"))]
    trace: HandleRefinementTrace,
}

impl HandleRefinementHooks {
    pub(crate) fn new(_session: u64) -> Self {
        Self {
            #[cfg(any(test, feature = "unstable"))]
            trace: HandleRefinementTrace::new(_session),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_before_seal_hook(
        &self,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) {
        self.trace.set_before_seal_hook(entered, release);
    }

    #[cfg(test)]
    pub(crate) fn trace_json(&self) -> String {
        self.trace.trace_json()
    }

    // These observer methods are intentionally available in every build. The
    // operational transition calls them unconditionally; only the façade
    // decides whether a refinement trace is compiled and records the event.
    pub(crate) fn observe_prepare(&self) -> PrepareObservation<'_> {
        self.observe_begin_prepare();
        PrepareObservation { hooks: self }
    }

    #[inline]
    pub(crate) fn observe_begin_prepare(&self) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.begin_prepare();
    }

    #[inline]
    fn observe_end_prepare(&self) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.end_prepare();
    }

    #[inline]
    pub(crate) fn observe_allocate_initializer_id(&self) -> u64 {
        #[cfg(any(test, feature = "unstable"))]
        return self.trace.allocate_initializer_id();
        #[cfg(not(any(test, feature = "unstable")))]
        0
    }

    #[inline]
    pub(crate) fn observe_begin_initializer(&self, key: &HandleTopicKey, runtime_id: u64) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.begin_initializer(key, runtime_id);
        let _ = (key, runtime_id);
    }

    #[inline]
    pub(crate) fn observe_insert_pending_fresh(&self, key: &HandleTopicKey, runtime_id: u64) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.insert_pending_fresh(key, runtime_id);
        let _ = (key, runtime_id);
    }

    #[inline]
    pub(crate) fn observe_insert_pending_reuse(
        &self,
        key: &HandleTopicKey,
        runtime_id: u64,
        slot: u64,
        generation: u64,
    ) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace
            .insert_pending_reuse(key, runtime_id, slot, generation);
        let _ = (key, runtime_id, slot, generation);
    }

    #[inline]
    pub(crate) fn observe_publish_and_install(
        &self,
        key: &HandleTopicKey,
        runtime_id: u64,
        token: TokenWire,
        rtd_key: &str,
    ) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace
            .publish_and_install(key, runtime_id, token, rtd_key);
        let _ = (key, runtime_id, token, rtd_key);
    }

    #[inline]
    pub(crate) fn observe_commit_and_activate(
        &self,
        key: &HandleTopicKey,
        runtime_id: u64,
        token: TokenWire,
    ) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace
            .linearize()
            .commit_and_activate(key, runtime_id, token);
        let _ = (key, runtime_id, token);
    }

    #[inline]
    pub(crate) fn observe_finish_initializer(&self, runtime_id: u64) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.finish_initializer(runtime_id);
        let _ = runtime_id;
    }

    #[inline]
    pub(crate) fn observe_withdraw_and_invalidate(
        &self,
        key: &HandleTopicKey,
        runtime_id: u64,
        token: TokenWire,
    ) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.withdraw_and_invalidate(key, runtime_id, token);
        let _ = (key, runtime_id, token);
    }

    #[inline]
    pub(crate) fn observe_rollback_pending(
        &self,
        key: &HandleTopicKey,
        runtime_id: u64,
        reusable: bool,
        token: TokenWire,
    ) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace
            .rollback_pending(key, runtime_id, reusable, token);
        let _ = (key, runtime_id, reusable, token);
    }

    #[inline]
    pub(crate) fn observe_begin_warm_read(&self, key: &HandleTopicKey) -> u64 {
        #[cfg(any(test, feature = "unstable"))]
        return self.trace.begin_warm_read(key);
        #[cfg(not(any(test, feature = "unstable")))]
        {
            let _ = key;
            0
        }
    }

    #[inline]
    pub(crate) fn observe_finish_warm_read(&self, reader_id: u64) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.finish_warm_read(reader_id);
        let _ = reader_id;
    }

    #[inline]
    pub(crate) fn observe_fail_warm_read(&self, reader_id: u64) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.fail_warm_read(reader_id);
        let _ = reader_id;
    }

    #[inline]
    pub(crate) fn observe_abandon_warm_read(&self, reader_id: u64) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.abandon_warm_read(reader_id);
        let _ = reader_id;
    }

    #[inline]
    pub(crate) fn observe_drain_pending(&self, token: TokenWire, runtime_id: u64, reusable: bool) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.drain_pending(token, runtime_id, reusable);
        let _ = (token, runtime_id, reusable);
    }

    #[inline]
    pub(crate) fn observe_drain_published(&self, token: TokenWire, reusable: bool) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.drain_published(token, reusable);
        let _ = (token, reusable);
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn observe_claim_server(&self, key: &HandleTopicKey, generation: ServerGeneration) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.claim_server(key, generation);
        let _ = (key, generation);
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn observe_begin_connection(&self, key: &HandleTopicKey, owner: HandleTopicOwner) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.begin_connection(key, owner);
        let _ = (key, owner);
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn observe_reuse_committed_connection(
        &self,
        key: &HandleTopicKey,
        owner: HandleTopicOwner,
    ) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.reuse_committed_connection(key, owner);
        let _ = (key, owner);
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn observe_commit_connection(&self, key: &HandleTopicKey, owner: HandleTopicOwner) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.commit_connection(key, owner);
        let _ = (key, owner);
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn observe_rollback_connection(
        &self,
        key: &HandleTopicKey,
        owner: HandleTopicOwner,
    ) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.rollback_connection(key, owner);
        let _ = (key, owner);
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn observe_disconnect(&self, key: &HandleTopicKey, owner: HandleTopicOwner) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.disconnect(key, owner);
        let _ = (key, owner);
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn observe_detach_generation(&self, generation: ServerGeneration) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.detach_generation(generation);
        let _ = generation;
    }

    pub(crate) fn observe_seal_for_close(&self) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.seal_for_close();
    }

    #[inline]
    pub(crate) fn observe_close_registry(&self) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.close_registry();
    }

    #[inline]
    pub(crate) fn observe_finish_close(&self) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.finish_close();
    }

    pub(crate) fn observe_mark_returned_success(&self) {
        #[cfg(any(test, feature = "unstable"))]
        self.trace.mark_returned_success();
    }
}

pub(crate) struct PrepareObservation<'a> {
    hooks: &'a HandleRefinementHooks,
}

impl Drop for PrepareObservation<'_> {
    fn drop(&mut self) {
        self.hooks.observe_end_prepare();
    }
}
