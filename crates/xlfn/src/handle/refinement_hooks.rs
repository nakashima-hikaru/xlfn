//! Feature-gated façade for the executable handle refinement trace.
//!
//! Operational handle code talks to this façade instead of depending on the
//! trace machine's storage type.  Production builds retain no trace state;
//! test and checker builds delegate to `refinement.rs`.

use super::HandleTopicKey;
use super::refinement_wire::TokenWire;
use super::store::HandleStore;
#[cfg(any(target_os = "windows", test))]
use super::{FormulaLifetimeGeneration, FormulaObserverId};

#[cfg(any(test, feature = "refinement"))]
use super::refinement::HandleRefinementTrace;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InitializerTraceId {
    #[cfg(any(test, feature = "refinement"))]
    id: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WarmReadTraceId {
    #[cfg(any(test, feature = "refinement"))]
    id: u64,
}

pub(crate) struct HandleRefinementHooks {
    #[cfg(any(test, feature = "refinement"))]
    trace: HandleRefinementTrace,
}

#[cfg(any(test, feature = "refinement"))]
impl HandleRefinementHooks {
    pub(crate) fn new(session: u64) -> Self {
        Self {
            trace: HandleRefinementTrace::new(session),
        }
    }

    /// Build observation-only payloads inside the same feature boundary as
    /// their consumers. Token authentication updates the thread-local token
    /// cache, so calling it before a no-op observer cannot be optimized away.
    #[inline]
    pub(crate) fn token_payload(&self, store: &HandleStore, token: &str) -> TokenWire {
        store.refinement_token(token)
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
        self.trace.begin_prepare();
    }

    #[inline]
    fn observe_end_prepare(&self) {
        self.trace.end_prepare();
    }

    #[inline]
    pub(crate) fn observe_allocate_initializer_id(&self) -> InitializerTraceId {
        InitializerTraceId {
            id: self.trace.allocate_initializer_id(),
        }
    }

    #[inline]
    pub(crate) fn observe_begin_initializer(
        &self,
        key: &HandleTopicKey,
        runtime_id: InitializerTraceId,
    ) {
        self.trace.begin_initializer(key, runtime_id.id);
    }

    #[inline]
    pub(crate) fn observe_insert_pending_fresh(
        &self,
        key: &HandleTopicKey,
        runtime_id: InitializerTraceId,
    ) {
        self.trace.insert_pending_fresh(key, runtime_id.id);
    }

    #[inline]
    pub(crate) fn observe_insert_pending_reuse(
        &self,
        key: &HandleTopicKey,
        runtime_id: InitializerTraceId,
        token: TokenWire,
    ) {
        self.trace
            .insert_pending_reuse(key, runtime_id.id, token.slot, token.generation);
    }

    #[inline]
    pub(crate) fn observe_publish_and_install(
        &self,
        key: &HandleTopicKey,
        runtime_id: InitializerTraceId,
        token: TokenWire,
        lifetime_key: &str,
    ) {
        self.trace
            .publish_and_install(key, runtime_id.id, token, lifetime_key);
    }

    #[inline]
    pub(crate) fn observe_commit_and_activate(
        &self,
        key: &HandleTopicKey,
        runtime_id: InitializerTraceId,
        token: TokenWire,
    ) {
        self.trace
            .linearize()
            .commit_and_activate(key, runtime_id.id, token);
    }

    #[inline]
    pub(crate) fn observe_finish_initializer(&self, runtime_id: InitializerTraceId) {
        self.trace.finish_initializer(runtime_id.id);
    }

    #[inline]
    pub(crate) fn observe_withdraw_and_invalidate(
        &self,
        key: &HandleTopicKey,
        runtime_id: InitializerTraceId,
        token: TokenWire,
    ) {
        self.trace
            .withdraw_and_invalidate(key, runtime_id.id, token);
    }

    #[inline]
    pub(crate) fn observe_rollback_pending(
        &self,
        key: &HandleTopicKey,
        runtime_id: InitializerTraceId,
        reusable: bool,
        token: TokenWire,
    ) {
        self.trace
            .rollback_pending(key, runtime_id.id, reusable, token);
    }

    #[inline]
    pub(crate) fn observe_begin_warm_read(&self, key: &HandleTopicKey) -> WarmReadTraceId {
        WarmReadTraceId {
            id: self.trace.begin_warm_read(key),
        }
    }

    #[inline]
    pub(crate) fn observe_finish_warm_read(&self, reader_id: WarmReadTraceId) {
        self.trace.finish_warm_read(reader_id.id);
    }

    #[inline]
    pub(crate) fn observe_fail_warm_read(&self, reader_id: WarmReadTraceId) {
        self.trace.fail_warm_read(reader_id.id);
    }

    #[inline]
    pub(crate) fn observe_abandon_warm_read(&self, reader_id: WarmReadTraceId) {
        self.trace.abandon_warm_read(reader_id.id);
    }

    #[inline]
    pub(crate) fn observe_drain_pending(
        &self,
        token: TokenWire,
        runtime_id: InitializerTraceId,
        reusable: bool,
    ) {
        self.trace.drain_pending(token, runtime_id.id, reusable);
    }

    #[inline]
    pub(crate) fn observe_drain_published(&self, token: TokenWire, reusable: bool) {
        self.trace.drain_published(token, reusable);
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn observe_claim_lifetime(
        &self,
        key: &HandleTopicKey,
        generation: FormulaLifetimeGeneration,
    ) {
        self.trace.claim_lifetime(key, generation);
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn observe_begin_connection(&self, key: &HandleTopicKey, owner: FormulaObserverId) {
        self.trace.begin_connection(key, owner);
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn observe_reuse_committed_connection(
        &self,
        key: &HandleTopicKey,
        owner: FormulaObserverId,
    ) {
        self.trace.reuse_committed_connection(key, owner);
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn observe_commit_connection(&self, key: &HandleTopicKey, owner: FormulaObserverId) {
        self.trace.commit_connection(key, owner);
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn observe_rollback_connection(
        &self,
        key: &HandleTopicKey,
        owner: FormulaObserverId,
    ) {
        self.trace.rollback_connection(key, owner);
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn observe_disconnect(&self, key: &HandleTopicKey, owner: FormulaObserverId) {
        self.trace.disconnect(key, owner);
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn observe_detach_generation(&self, generation: FormulaLifetimeGeneration) {
        self.trace.detach_generation(generation);
    }

    pub(crate) fn observe_seal_for_close(&self) {
        self.trace.seal_for_close();
    }

    #[inline]
    pub(crate) fn observe_close_registry(&self) {
        self.trace.close_registry();
    }

    #[inline]
    pub(crate) fn observe_finish_close(&self) {
        self.trace.finish_close();
    }

    pub(crate) fn observe_mark_returned_success(&self) {
        self.trace.mark_returned_success();
    }
}

#[cfg(not(any(test, feature = "refinement")))]
impl HandleRefinementHooks {
    pub(crate) fn new(_: u64) -> Self {
        Self {}
    }

    #[inline]
    pub(crate) fn token_payload(&self, _: &HandleStore, _: &str) -> TokenWire {
        TokenWire {}
    }

    #[inline]
    pub(crate) fn observe_prepare(&self) -> PrepareObservation<'_> {
        PrepareObservation {
            _hooks: std::marker::PhantomData,
        }
    }

    #[inline]
    pub(crate) fn observe_allocate_initializer_id(&self) -> InitializerTraceId {
        InitializerTraceId {}
    }

    #[inline]
    pub(crate) fn observe_begin_initializer(&self, _: &HandleTopicKey, _: InitializerTraceId) {}

    #[inline]
    pub(crate) fn observe_insert_pending_fresh(&self, _: &HandleTopicKey, _: InitializerTraceId) {}

    #[inline]
    pub(crate) fn observe_insert_pending_reuse(
        &self,
        _: &HandleTopicKey,
        _: InitializerTraceId,
        _: TokenWire,
    ) {
    }

    #[inline]
    pub(crate) fn observe_publish_and_install(
        &self,
        _: &HandleTopicKey,
        _: InitializerTraceId,
        _: TokenWire,
        _: &str,
    ) {
    }

    #[inline]
    pub(crate) fn observe_commit_and_activate(
        &self,
        _: &HandleTopicKey,
        _: InitializerTraceId,
        _: TokenWire,
    ) {
    }

    #[inline]
    pub(crate) fn observe_finish_initializer(&self, _: InitializerTraceId) {}

    #[inline]
    pub(crate) fn observe_withdraw_and_invalidate(
        &self,
        _: &HandleTopicKey,
        _: InitializerTraceId,
        _: TokenWire,
    ) {
    }

    #[inline]
    pub(crate) fn observe_rollback_pending(
        &self,
        _: &HandleTopicKey,
        _: InitializerTraceId,
        _: bool,
        _: TokenWire,
    ) {
    }

    #[inline]
    pub(crate) fn observe_begin_warm_read(&self, _: &HandleTopicKey) -> WarmReadTraceId {
        WarmReadTraceId {}
    }

    #[inline]
    pub(crate) fn observe_finish_warm_read(&self, _: WarmReadTraceId) {}

    #[inline]
    pub(crate) fn observe_fail_warm_read(&self, _: WarmReadTraceId) {}

    #[inline]
    pub(crate) fn observe_abandon_warm_read(&self, _: WarmReadTraceId) {}

    #[cfg(target_os = "windows")]
    pub(crate) fn observe_claim_lifetime(&self, _: &HandleTopicKey, _: FormulaLifetimeGeneration) {}

    #[cfg(target_os = "windows")]
    pub(crate) fn observe_begin_connection(&self, _: &HandleTopicKey, _: FormulaObserverId) {}

    #[cfg(target_os = "windows")]
    pub(crate) fn observe_reuse_committed_connection(
        &self,
        _: &HandleTopicKey,
        _: FormulaObserverId,
    ) {
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn observe_commit_connection(&self, _: &HandleTopicKey, _: FormulaObserverId) {}

    #[cfg(target_os = "windows")]
    pub(crate) fn observe_rollback_connection(&self, _: &HandleTopicKey, _: FormulaObserverId) {}

    #[cfg(target_os = "windows")]
    pub(crate) fn observe_detach_generation(&self, _: FormulaLifetimeGeneration) {}

    pub(crate) fn observe_seal_for_close(&self) {}

    #[inline]
    pub(crate) fn observe_close_registry(&self) {}

    #[inline]
    pub(crate) fn observe_finish_close(&self) {}

    pub(crate) fn observe_mark_returned_success(&self) {}
}

pub(crate) struct PrepareObservation<'a> {
    #[cfg(any(test, feature = "refinement"))]
    hooks: &'a HandleRefinementHooks,
    #[cfg(not(any(test, feature = "refinement")))]
    _hooks: std::marker::PhantomData<&'a HandleRefinementHooks>,
}

#[cfg(any(test, feature = "refinement"))]
impl Drop for PrepareObservation<'_> {
    fn drop(&mut self) {
        self.hooks.observe_end_prepare();
    }
}
