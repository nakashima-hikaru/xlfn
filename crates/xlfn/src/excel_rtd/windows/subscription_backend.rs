//! Concrete copied subscription handles remain typed through COM dispatch.
//! A server stores its handle inline and clears it after terminal quiescence.
//! Connections stay concrete through VARIANT publication and transaction commit.

use super::automation::write_value_variant;
use super::server::DeferredDisconnectLease;
use super::update_event::RtdNotifier;
use crate::subscription::{
    RtdRefreshBatch, ServerGeneration, SubscriptionKey, SubscriptionRuntime,
    SubscriptionServerHandle, TopicId,
};
use crate::win32::{E_FAIL, S_OK, VARIANT, VariantClear};
use crate::{RtdSubscription, XllResult};
use std::ptr::NonNull;

#[cfg(test)]
pub(super) static FAIL_NEXT_SUBSCRIPTION_WRITE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(super) struct BackendSubscriptions<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
>(NonNull<SubscriptionRuntime<E, C>>);
impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Copy for BackendSubscriptions<E, C> {}
impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Clone for BackendSubscriptions<E, C> {
    fn clone(&self) -> Self {
        *self
    }
}
// SAFETY: COM admission and generation shutdown retain the synchronized runtime.
unsafe impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Send for BackendSubscriptions<E, C> {}
// SAFETY: SubscriptionRuntime serializes operations on the shared pointer.
unsafe impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Sync for BackendSubscriptions<E, C> {}
impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> BackendSubscriptions<E, C> {
    pub(super) fn new(runtime: &SubscriptionRuntime<E, C>) -> Self {
        Self(NonNull::from(runtime))
    }
    pub(super) fn same_runtime(self, other: Self) -> bool {
        self.0 == other.0
    }
    #[cfg(test)]
    pub(super) fn is_runtime(&self, runtime: &SubscriptionRuntime<E, C>) -> bool {
        self.same_runtime(Self::new(runtime))
    }
    /// # Safety
    /// Retain this runtime through every method call on the returned copied handle.
    pub(super) unsafe fn register_server(
        &self,
        generation: ServerGeneration,
    ) -> XllResult<BackendSubscriptionServer<E, C>> {
        // SAFETY: the caller retains the runtime through copied-handle use.
        let handle = unsafe { self.0.as_ref().register_server(generation)? };
        Ok(BackendSubscriptionServer(handle))
    }
}

/// Copies a concrete non-owning server handle; no adapter allocation is required.
pub(super) struct BackendSubscriptionServer<
    E: RtdSubscription = (),
    C: crate::RtdSourceCatalog<E> = crate::DynamicRtdSources<E>,
>(SubscriptionServerHandle<E, C>);
impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Copy for BackendSubscriptionServer<E, C> {}
impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> Clone for BackendSubscriptionServer<E, C> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<E: RtdSubscription, C: crate::RtdSourceCatalog<E>> BackendSubscriptionServer<E, C> {
    pub(super) fn claim(&self, key: SubscriptionKey) -> XllResult<()> {
        self.0.claim(self.0.resolve_transport_key(key)?)
    }
    pub(super) unsafe fn connect_data(
        &self,
        topic_id: TopicId,
        key: SubscriptionKey,
        result: *mut VARIANT,
    ) -> i32 {
        let connection = self
            .0
            .resolve_transport_key(key)
            .and_then(|id| self.0.connect_transaction(topic_id, id));
        let connection = match connection {
            Ok(connection) => connection,
            Err(error) => {
                crate::diagnostics::report_no_unwind("IRtdServer::ConnectData", &error);
                return E_FAIL;
            }
        };
        // SAFETY: ConnectData validated result. The concrete connection owns
        // rollback until its value is written and its typed commit succeeds.
        unsafe {
            finish_connect_data(
                connection,
                result,
                |connection, result| {
                    #[cfg(test)]
                    if FAIL_NEXT_SUBSCRIPTION_WRITE.swap(false, std::sync::atomic::Ordering::AcqRel)
                    {
                        return E_FAIL;
                    }
                    write_value_variant(result, connection.value())
                },
                |connection| connection.commit(),
            )
        }
    }

    pub(super) fn attach_update_notifier(
        &self,
        notifier: RtdNotifier,
    ) -> XllResult<Option<RtdNotifier>> {
        self.0.attach_update_notifier(notifier)
    }
    pub(super) fn detach_update_notifier(&self) -> Option<RtdNotifier> {
        self.0.detach_update_notifier()
    }
    pub(super) fn pulse_notification(&self) -> XllResult<()> {
        self.0.pulse_notification()
    }
    pub(super) fn begin_refresh(&self) -> XllResult<RtdRefreshBatch<'_>> {
        self.0.begin_refresh()
    }
    pub(super) fn disconnect_deferred(
        &self,
        topic_id: TopicId,
        completion: DeferredDisconnectLease,
    ) -> XllResult<()> {
        self.0.disconnect_deferred(topic_id, completion)
    }
    pub(super) fn terminate(&self) -> XllResult<()> {
        self.0.terminate()
    }
    #[cfg(test)]
    pub(super) fn pending_update_count(&self) -> usize {
        self.0.pending_update_count()
    }
}

/// Keep a concrete transaction owned until conversion succeeds; commit failures
/// release initialized VARIANT resources before reporting failure to COM.
///
/// # Safety
/// `result` must be valid writable VARIANT storage. `write` initializes it and
/// returns S_OK only when it is safe to pass that value to VariantClear.
pub(super) unsafe fn finish_connect_data<C>(
    connection: C,
    result: *mut VARIANT,
    write: impl FnOnce(&C, *mut VARIANT) -> i32,
    commit: impl FnOnce(C) -> XllResult<()>,
) -> i32 {
    let status = write(&connection, result);
    if status != S_OK {
        drop(connection);
        return status;
    }
    let initialized = scopeguard::guard(result, |result| {
        // SAFETY: write reported an initialized VARIANT. Keep ownership until
        // commit succeeds so both an error and unwinding clear its resources.
        let _ = unsafe { VariantClear(result) };
    });
    if let Err(error) = commit(connection) {
        crate::diagnostics::report_no_unwind("IRtdServer::ConnectData commit", &error);
        return E_FAIL;
    }
    let _ = scopeguard::ScopeGuard::into_inner(initialized);
    S_OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::excel_rtd::windows::automation::write_bstr_variant;
    use crate::excel_rtd::windows::com_abi::VT_EMPTY;
    use crate::panic_boundary::catch_no_unwind;
    use std::panic::AssertUnwindSafe;

    #[test]
    fn initialized_variant_is_cleared_when_commit_errors_or_unwinds() {
        for panics in [false, true] {
            let mut result = VARIANT::default();
            let status = catch_no_unwind(AssertUnwindSafe(|| {
                // SAFETY: result is writable COM storage and the successful
                // writer initializes an owned BSTR before commit fails.
                unsafe {
                    finish_connect_data(
                        (),
                        &mut result,
                        |_, result| write_bstr_variant(result, "rollback-owned-bstr"),
                        |()| {
                            assert!(!panics, "injected commit panic");
                            Err(crate::XllError::Overloaded)
                        },
                    )
                }
            }));
            if panics {
                assert!(status.is_err());
            } else {
                assert_eq!(status.unwrap(), E_FAIL);
            }
            // SAFETY: VariantClear from the initialized-value guard must have
            // released the owned BSTR and reset the initialized discriminant.
            unsafe { assert_eq!(result.Anonymous.Anonymous.vt, VT_EMPTY) };
        }
    }
}
