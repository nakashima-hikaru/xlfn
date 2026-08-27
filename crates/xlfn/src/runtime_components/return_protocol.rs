//! Excel return ownership and call/calculation identity protocol.

use std::sync::atomic::AtomicU64;

/// Excel return ownership and call/calculation identity state.
pub(crate) struct ReturnProtocol {
    pub(crate) returns: crate::return_abi::ReturnTracker,
    pub(crate) next_call_id: AtomicU64,
    #[cfg(not(feature = "async"))]
    pub(crate) calculation_id: AtomicU64,
}

impl ReturnProtocol {
    pub(crate) const fn new() -> Self {
        Self {
            returns: crate::return_abi::ReturnTracker::new_closed(),
            next_call_id: AtomicU64::new(1),
            #[cfg(not(feature = "async"))]
            calculation_id: AtomicU64::new(1),
        }
    }

    pub(crate) fn close_admission(&self) {
        self.returns.close_admission();
    }

    pub(crate) fn reopen_admission(&self) -> crate::XllResult<()> {
        self.returns.reopen_admission()
    }

    pub(crate) fn enter_producer(
        &'static self,
    ) -> Option<crate::return_abi::ReturnProducerGuard<'static>> {
        self.returns.try_enter_producer()
    }

    pub(crate) fn wait_for_returns(&self) {
        self.returns.wait_for_quiescence();
    }

    pub(crate) fn returns_are_quiescent(&self) -> bool {
        self.returns.is_quiescent()
    }

    pub(crate) fn returns_closed_and_quiescent(&self) -> bool {
        self.returns.admission_closed() && self.returns.is_quiescent()
    }

    pub(crate) fn next_call_id(&self) -> u64 {
        self.next_call_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn peek_next_call_id(&self) -> u64 {
        self.next_call_id.load(std::sync::atomic::Ordering::Relaxed)
    }
}
