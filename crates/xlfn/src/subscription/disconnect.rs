//! Generation-owned cleanup for COM DisconnectData. The caller revokes the
//! topic immediately; this worker joins its producers without blocking an STA.

use super::host::SubscriptionHost;
use super::server::{OwnedServerOperation, disconnect_one_no_unwind};
use super::source::RtdSubscription;
use crate::panic_boundary::catch_no_unwind;
use crate::sync::{Condvar, Mutex};
use crate::{XllError, XllResult};
use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::thread::{Builder, JoinHandle};
use triomphe::Arc;

pub(super) struct DeferredDisconnect<H: SubscriptionHost> {
    subscription: Option<Box<dyn RtdSubscription>>,
    // The connection reserved this capacity before any source sink escaped.
    _reservation: DisconnectReservation<H>,
    // Drop the external server/module lease before releasing runtime admission.
    _completion: Box<dyn Send>,
    operation: OwnedServerOperation<H>,
}

impl<H: SubscriptionHost> DeferredDisconnect<H> {
    pub(super) fn new(
        subscription: Box<dyn RtdSubscription>,
        completion: Box<dyn Send>,
        reservation: DisconnectReservation<H>,
        operation: OwnedServerOperation<H>,
    ) -> Self {
        Self {
            subscription: Some(subscription),
            _reservation: reservation,
            _completion: completion,
            operation,
        }
    }

    fn finish(mut self) {
        let subscription = self
            .subscription
            .take()
            .expect("cleanup owns a subscription");
        let cancel = catch_no_unwind(AssertUnwindSafe(|| subscription.request_cancel()))
            .map_err(|_| XllError::Panic);
        // Even a panicking cancellation must finish the sink-user barrier.
        let disconnect = disconnect_one_no_unwind(subscription);
        let result = cancel.and(disconnect);
        if let Err(error) = &result {
            crate::diagnostics::report_no_unwind("RTD deferred disconnect", error);
        }
        self.operation
            .server()
            .publish
            .services()
            .record_cleanup_result(result);
    }
}

struct Ready<H: SubscriptionHost> {
    jobs: VecDeque<DeferredDisconnect<H>>,
    outstanding: usize,
    stopping: bool,
}

struct DisconnectQueue<H: SubscriptionHost> {
    ready: Mutex<Ready<H>>,
    changed: Condvar,
}

impl<H: SubscriptionHost> DisconnectQueue<H> {
    fn run(&self) {
        loop {
            let job = {
                let mut ready = self.ready.lock();
                while ready.jobs.is_empty() && !ready.stopping {
                    self.changed.wait(&mut ready);
                }
                match ready.jobs.pop_front() {
                    Some(job) => job,
                    None => return,
                }
            };
            job.finish();
        }
    }
}

enum DisconnectWorker<H: SubscriptionHost> {
    Dormant,
    Running(Arc<DisconnectQueue<H>>, JoinHandle<()>),
    Stopped,
}

/// One lazy worker per generation. Every live or retiring source owner reserves
/// capacity before subscription starts, so terminal disconnect never competes
/// with new connections for cleanup capacity or worker startup. Runtime
/// admission keeps every queued job alive until its sink-user barrier finishes.
pub(super) struct DisconnectPool<H: SubscriptionHost> {
    worker: Mutex<DisconnectWorker<H>>,
    capacity: usize,
    #[cfg(test)]
    fail_spawn: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_initialize: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_stop_preflight: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    stop_attempts: std::sync::atomic::AtomicUsize,
}

impl<H: SubscriptionHost> DisconnectPool<H> {
    pub(super) const fn new(capacity: usize) -> Self {
        Self {
            worker: Mutex::new(DisconnectWorker::Dormant),
            capacity,
            #[cfg(test)]
            fail_spawn: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_initialize: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_stop_preflight: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            stop_attempts: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub(super) fn reserve(&self) -> XllResult<DisconnectReservation<H>> {
        if self.capacity == 0 {
            return Err(XllError::Overloaded);
        }
        let mut worker = self.worker.lock();
        if matches!(*worker, DisconnectWorker::Dormant) {
            let queue = Arc::new(DisconnectQueue {
                ready: Mutex::new(Ready {
                    jobs: VecDeque::new(),
                    outstanding: 0,
                    stopping: false,
                }),
                changed: Condvar::new(),
            });
            #[cfg(test)]
            if self
                .fail_spawn
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                return Err(XllError::Overloaded);
            }
            let shared = Arc::clone(&queue);
            let (started, startup) = std::sync::mpsc::sync_channel(1);
            #[cfg(test)]
            let fail_initialize = self
                .fail_initialize
                .swap(false, std::sync::atomic::Ordering::Relaxed);
            let handle = Builder::new()
                .name("xlfn-rtd-disconnect".into())
                .spawn(move || {
                    #[cfg(test)]
                    if fail_initialize {
                        let _ = started.send(Err(XllError::Overloaded));
                        return;
                    }
                    #[cfg(all(windows, any(feature = "rtd", feature = "handles")))]
                    let _apartment = match crate::excel_rtd::ComApartmentGuard::enter() {
                        Ok(apartment) => apartment,
                        Err(code) => {
                            let _ = started.send(Err(XllError::Native {
                                code,
                                message: "failed to initialize RTD cleanup COM apartment".into(),
                            }));
                            return;
                        }
                    };
                    if started.send(Ok(())).is_ok() {
                        shared.run();
                    }
                })
                .map_err(|error| XllError::Native {
                    code: error.raw_os_error().unwrap_or(0),
                    message: format!("failed to start RTD disconnect worker: {error}"),
                })?;
            // Publish the queue only after worker startup succeeds. Failure
            // joins its empty worker before a source acquires any sink users.
            if let Err(error) = startup.recv().unwrap_or(Err(XllError::Panic)) {
                // The failure paths have not acquired a COM apartment or
                // admitted a job, so this join has no STA dispatch dependency.
                let _ = crate::panic_boundary::contain_panic(handle.join());
                return Err(error);
            }
            *worker = DisconnectWorker::Running(queue, handle);
        }
        let DisconnectWorker::Running(queue, _) = &*worker else {
            return Err(XllError::Closing);
        };
        let mut ready = queue.ready.lock();
        if ready.outstanding >= self.capacity {
            return Err(XllError::Overloaded);
        }
        // Reserve the eventual queue slot now, including slots for all live
        // owners. Enqueue after terminal revocation requires no allocation.
        let additional = ready.outstanding + 1 - ready.jobs.len();
        ready
            .jobs
            .try_reserve(additional)
            .map_err(|_| XllError::Overloaded)?;
        ready.outstanding += 1;
        Ok(DisconnectReservation {
            queue: Arc::clone(queue),
        })
    }

    #[allow(
        clippy::drop_non_drop,
        reason = "explicitly release an unexecuted join closure's affine worker borrows before restoring ownership"
    )]
    pub(super) fn stop(&self) -> XllResult<()> {
        // Runtime close owns the single stop sequence and has drained every
        // reserve/cleanup admission. Release this lock before STA dispatch.
        let retiring = {
            let mut worker = self.worker.lock();
            std::mem::replace(&mut *worker, DisconnectWorker::Stopped)
        };
        if let DisconnectWorker::Running(queue, handle) = retiring {
            #[cfg(test)]
            self.stop_attempts
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut owned_handle = Some(handle);
            let join = || {
                // Only start retirement after the dispatch helper is ready.
                // Its preflight failure returns this unexecuted closure, so
                // the still-running worker and its handle can be restored.
                queue.ready.lock().stopping = true;
                queue.changed.notify_all();
                if crate::panic_boundary::contain_panic(owned_handle.take().unwrap().join())
                    .is_err()
                {
                    // Losing a worker can strand owned admissions and sink users.
                    xlfn_kernel::invariant::fail_stop();
                }
            };
            #[cfg(test)]
            if self
                .fail_stop_preflight
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                // Model the helper returning an unused join on preflight
                // failure; exercise the same ownership recovery on every OS.
                drop(join);
                return self.retain_worker(
                    queue,
                    owned_handle.take().unwrap(),
                    XllError::Overloaded,
                );
            }
            // The final job releases runtime admission before this worker's
            // CoUninitialize. Keep the closing STA dispatchable through that
            // last COM teardown as well as the sink-user barriers.
            #[cfg(all(windows, any(feature = "rtd", feature = "handles")))]
            if let Err((error, unused_join)) = crate::excel_rtd::drain_with_com_dispatch(join) {
                drop(unused_join);
                return self.retain_worker(queue, owned_handle.take().unwrap(), error);
            }
            #[cfg(not(all(windows, any(feature = "rtd", feature = "handles"))))]
            {
                let mut join = join;
                join();
            }
            debug_assert_eq!(queue.ready.lock().outstanding, 0);
        }
        Ok(())
    }

    #[cfg(any(test, all(windows, any(feature = "rtd", feature = "handles"))))]
    fn retain_worker(
        &self,
        queue: Arc<DisconnectQueue<H>>,
        handle: JoinHandle<()>,
        error: XllError,
    ) -> XllResult<()> {
        *self.worker.lock() = DisconnectWorker::Running(queue, handle);
        Err(error)
    }

    #[cfg(test)]
    pub(super) fn fail_next_spawn(&self) {
        self.fail_spawn
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(super) fn fail_next_initialize(&self) {
        self.fail_initialize
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(super) fn retained_source_count(&self) -> usize {
        match &*self.worker.lock() {
            DisconnectWorker::Running(queue, _) => queue.ready.lock().outstanding,
            _ => 0,
        }
    }
}

impl<H: SubscriptionHost> Drop for DisconnectPool<H> {
    fn drop(&mut self) {
        if self.stop().is_err() {
            // A normal teardown retains the runtime on preflight failure.
            // Destruction has no owner to quarantine this worker under.
            xlfn_kernel::invariant::fail_stop();
        }
    }
}

pub(super) struct DisconnectReservation<H: SubscriptionHost> {
    queue: Arc<DisconnectQueue<H>>,
}

impl<H: SubscriptionHost> DisconnectReservation<H> {
    fn commit(
        self,
        subscription: Box<dyn RtdSubscription>,
        completion: Box<dyn Send>,
        operation: OwnedServerOperation<H>,
    ) {
        let queue = Arc::clone(&self.queue);
        queue.ready.lock().jobs.push_back(DeferredDisconnect::new(
            subscription,
            completion,
            self,
            operation,
        ));
        queue.changed.notify_one();
    }
}

impl<H: SubscriptionHost> Drop for DisconnectReservation<H> {
    fn drop(&mut self) {
        let mut ready = self.queue.ready.lock();
        ready.outstanding = ready
            .outstanding
            .checked_sub(1)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
    }
}

/// Unique source owner and its already prepared terminal cleanup capacity.
/// Keep the reservation through every synchronous and asynchronous barrier,
/// including rollback, cleanup errors, and unwinding of a termination owner.
pub(super) struct OwnedSubscription<H: SubscriptionHost> {
    subscription: Option<Box<dyn RtdSubscription>>,
    reservation: Option<DisconnectReservation<H>>,
}

impl<H: SubscriptionHost> OwnedSubscription<H> {
    pub(super) fn new(
        subscription: Box<dyn RtdSubscription>,
        reservation: DisconnectReservation<H>,
    ) -> Self {
        Self {
            subscription: Some(subscription),
            reservation: Some(reservation),
        }
    }

    pub(super) fn request_cancel(&self) {
        self.subscription
            .as_ref()
            .expect("owner retains its subscription")
            .request_cancel();
    }

    pub(super) fn disconnect_and_wait(mut self) -> XllResult<()> {
        disconnect_one_no_unwind(
            self.subscription
                .take()
                .expect("owner retains its subscription"),
        )
    }

    pub(super) fn defer(mut self, completion: Box<dyn Send>, operation: OwnedServerOperation<H>) {
        self.reservation
            .take()
            .expect("owner retains its cleanup capacity")
            .commit(
                self.subscription
                    .take()
                    .expect("owner retains its subscription"),
                completion,
                operation,
            );
    }

    #[cfg(test)]
    pub(super) fn map_subscription_for_test(
        mut self,
        map: impl FnOnce(Box<dyn RtdSubscription>) -> Box<dyn RtdSubscription>,
    ) -> Self {
        self.subscription = Some(map(self.subscription.take().unwrap()));
        self
    }
}

impl<H: SubscriptionHost> Drop for OwnedSubscription<H> {
    fn drop(&mut self) {
        if let Some(subscription) = self.subscription.take()
            && let Err(error) = disconnect_one_no_unwind(subscription)
        {
            crate::diagnostics::report_no_unwind("RTD owned subscription drop", &error);
        }
    }
}

#[cfg(all(test, feature = "rtd"))]
mod tests {
    #![allow(
        unsafe_code,
        reason = "test sources retain sinks only until their explicit disconnect barrier"
    )]

    use super::*;
    use crate::excel_rtd::{RtdNotifier, RtdSubscriptionHost};
    use crate::generation::{ConnectionGeneration, RuntimeGeneration};
    use crate::rtd::test_support::TestNotifierState;
    use crate::subscription::runtime::SubscriptionRuntime;
    use crate::subscription::{
        RtdChannelSource, RtdSender, RtdSink, RtdSource, RtdTopic, SourceRegistration,
        StoredRtdValue, TopicId,
    };
    use std::num::NonZeroUsize;
    use std::sync::Arc as StdArc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    const DEADLINE: Duration = Duration::from_secs(5);

    struct Completion(StdArc<AtomicBool>);

    impl Drop for Completion {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    type NotifyCallback = Box<dyn FnOnce() + Send>;

    #[derive(Clone)]
    struct ReentrantHost(StdArc<Mutex<Option<NotifyCallback>>>);

    impl SubscriptionHost for ReentrantHost {
        type AdmissionGuard = ();
        type Notifier = ();

        fn enter_with<F>(&self, operation: F) -> XllResult<()>
        where
            F: FnOnce() -> XllResult<()>,
        {
            operation()
        }

        fn notify(&self, _: &()) -> XllResult<()> {
            let callback = self.0.lock().take();
            if let Some(callback) = callback {
                callback();
            }
            Ok(())
        }
    }

    fn channel_runtime<H: SubscriptionHost>(
        host: H,
    ) -> (
        StdArc<SubscriptionRuntime<H>>,
        crate::subscription::server::SubscriptionServerHandle<H>,
        RtdSender<i32>,
    ) {
        let registration = SourceRegistration::new(RuntimeGeneration::new(1).unwrap());
        let (sender_tx, sender_rx) = mpsc::sync_channel(1);
        let source = registration
            .register(RtdChannelSource::new(
                NonZeroUsize::new(64).unwrap(),
                move |_| {
                    let sender_tx = sender_tx.clone();
                    Ok(move |sender: RtdSender<i32>| {
                        sender_tx.send(sender.clone()).unwrap();
                        assert!(sender.wait_closed(DEADLINE));
                        Ok(())
                    })
                },
            ))
            .unwrap();
        let runtime = StdArc::new(SubscriptionRuntime::with_host(
            RuntimeGeneration::new(1).unwrap(),
            crate::subscription::RtdLimits::standard(),
            host,
            registration.finish(),
        ));
        let server = runtime.register_test_server(1);
        let prepared = runtime
            .prepare(&source, RtdTopic::single("deferred").unwrap().borrowed())
            .unwrap();
        server
            .connect_transaction(TopicId(1), prepared.id())
            .unwrap()
            .commit()
            .unwrap();
        prepared.commit();
        let sender = sender_rx.recv_timeout(DEADLINE).unwrap();
        (runtime, server, sender)
    }

    #[test]
    fn deferred_disconnect_reenters_from_channel_notification_without_waiting_for_it() {
        let callback = StdArc::new(Mutex::new(None));
        let (runtime, server, sender) = channel_runtime(ReentrantHost(StdArc::clone(&callback)));
        let completed = StdArc::new(AtomicBool::new(false));
        let lease = StdArc::clone(&completed);
        let (returned_tx, returned_rx) = mpsc::sync_channel(1);
        *callback.lock() = Some(Box::new(move || {
            // This executes while the real channel publisher holds its
            // publication mutex. A synchronous disconnect self-deadlocks.
            server
                .disconnect_deferred(TopicId(1), Box::new(Completion(lease)))
                .unwrap();
            assert!(matches!(
                server.test_server().publish.publish(
                    TopicId(1),
                    ConnectionGeneration::new(1).unwrap(),
                    StoredRtdValue::Integer(99),
                ),
                Err(XllError::Closing)
            ));
            returned_tx.send(()).unwrap();
        }));
        server.attach_update_notifier(()).unwrap();
        sender.try_send(1).unwrap();
        returned_rx.recv_timeout(DEADLINE).unwrap();
        runtime.close().unwrap();
        assert!(completed.load(Ordering::Acquire));
        assert!(sender.is_closed());
    }

    #[test]
    fn deferred_disconnect_returns_before_blocked_notification_and_close_drains_it() {
        let (runtime, server, sender) = channel_runtime(RtdSubscriptionHost::detached());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let notifier = StdArc::new(TestNotifierState::new());
        *notifier.entered.lock() = Some(entered_tx);
        *notifier.release.lock() = Some(release_rx);
        server
            .attach_update_notifier(RtdNotifier::for_test(notifier))
            .unwrap();
        sender.try_send(1).unwrap();
        entered_rx.recv_timeout(DEADLINE).unwrap();
        let completed = StdArc::new(AtomicBool::new(false));
        server
            .disconnect_deferred(TopicId(1), Box::new(Completion(StdArc::clone(&completed))))
            .unwrap();
        assert!(!completed.load(Ordering::Acquire));
        assert_eq!(server.pending_update_count(), 0);
        assert!(sender.wait_closed(DEADLINE));

        let closing_runtime = StdArc::clone(&runtime);
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closing = std::thread::spawn(move || closed_tx.send(closing_runtime.close()).unwrap());
        // The notification and cleanup leases both retain the runtime. The
        // caller can now release the simulated STA dispatch dependency.
        assert!(closed_rx.try_recv().is_err());
        release_tx.send(()).unwrap();
        closed_rx.recv_timeout(DEADLINE).unwrap().unwrap();
        closing.join().unwrap();
        assert!(completed.load(Ordering::Acquire));
    }

    struct FixtureSource {
        sink: StdArc<Mutex<Option<RtdSink<i32>>>>,
        disconnected: StdArc<AtomicBool>,
        fail_cancel: bool,
        fail_disconnect: bool,
    }

    struct FixtureSubscription {
        sink: StdArc<Mutex<Option<RtdSink<i32>>>>,
        disconnected: StdArc<AtomicBool>,
        fail_cancel: bool,
        fail_disconnect: bool,
    }

    // SAFETY: the subscription clears its only retained sink before returning,
    // including either injected error path; no publisher is detached.
    unsafe impl RtdSubscription for FixtureSubscription {
        fn request_cancel(&self) {
            assert!(!self.fail_cancel, "injected cancellation panic");
        }

        fn disconnect_and_wait(self: Box<Self>) -> XllResult<()> {
            self.sink.lock().take();
            self.disconnected.store(true, Ordering::Release);
            if self.fail_disconnect {
                Err(XllError::Overloaded)
            } else {
                Ok(())
            }
        }
    }

    // SAFETY: only the returned subscription owns reclamation of the sink slot.
    unsafe impl RtdSource for FixtureSource {
        type Value = i32;
        type Subscription = FixtureSubscription;

        fn subscribe(&self, _: &RtdTopic, sink: RtdSink<i32>) -> XllResult<FixtureSubscription> {
            *self.sink.lock() = Some(sink);
            Ok(FixtureSubscription {
                sink: StdArc::clone(&self.sink),
                disconnected: StdArc::clone(&self.disconnected),
                fail_cancel: self.fail_cancel,
                fail_disconnect: self.fail_disconnect,
            })
        }
    }

    #[test]
    fn connection_prepares_terminal_cleanup_before_starting_its_source() {
        for failure in ["capacity", "spawn", "initialize"] {
            let (arena, source, sink_slot, disconnected) =
                crate::subscription::tests::publishing_source::<i32>(None);
            let runtime = SubscriptionRuntime::with_host(
                RuntimeGeneration::new(1).unwrap(),
                crate::subscription::RtdLimits::standard().with_max_active(
                    crate::subscription::RtdCapacity::bounded(NonZeroUsize::new(1).unwrap()),
                ),
                RtdSubscriptionHost::detached(),
                arena,
            );
            let server = runtime.register_test_server(1);
            let prepared = runtime
                .prepare(&source, RtdTopic::single("prepared").unwrap().borrowed())
                .unwrap();
            let reservation = match failure {
                "spawn" => {
                    runtime.disconnects.fail_next_spawn();
                    None
                }
                "initialize" => {
                    runtime.disconnects.fail_next_initialize();
                    None
                }
                _ => Some(runtime.disconnects.reserve().unwrap()),
            };
            assert!(matches!(
                server.connect_transaction(TopicId(1), prepared.id()),
                Err(XllError::Overloaded)
            ));
            assert!(
                sink_slot.lock().is_none(),
                "source must not start on cleanup setup failure"
            );
            assert!(!disconnected.load(Ordering::Acquire));
            assert_eq!(runtime.services.active_quota.used(), 0);
            assert_eq!(
                runtime.disconnects.retained_source_count(),
                usize::from(reservation.is_some())
            );
            drop(reservation);
            server
                .connect_transaction(TopicId(1), prepared.id())
                .unwrap()
                .commit()
                .unwrap();
            prepared.commit();
            assert_eq!(runtime.disconnects.retained_source_count(), 1);
            // Startup failure and capacity admission now happen before connect;
            // the owner's single terminal disconnect always consumes its slot.
            server
                .disconnect_deferred(TopicId(1), Box::new(()))
                .unwrap();
            runtime.close().unwrap();
            assert!(disconnected.load(Ordering::Acquire));
            assert!(sink_slot.lock().is_none());
            assert_eq!(runtime.services.active_quota.used(), 0);
            assert_eq!(runtime.disconnects.retained_source_count(), 0);
        }
    }

    struct CleanupFinished(mpsc::SyncSender<()>);
    impl Drop for CleanupFinished {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    #[test]
    fn terminal_disconnect_capacity_stays_with_the_source_until_join() {
        let calls = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
        let started = StdArc::clone(&calls);
        let (sender_tx, sender_rx) = mpsc::channel();
        let (joining_tx, joining_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release = StdArc::new(Mutex::new(Some(release_rx)));
        let registration = SourceRegistration::new(RuntimeGeneration::new(1).unwrap());
        let source = registration
            .register(
                RtdChannelSource::new(NonZeroUsize::new(8).unwrap(), move |_| {
                    let sequence = started.fetch_add(1, Ordering::Relaxed);
                    let sender_tx = sender_tx.clone();
                    let joining_tx = joining_tx.clone();
                    let release = StdArc::clone(&release);
                    Ok(move |sender: RtdSender<i32>| {
                        sender_tx.send(sender.clone()).unwrap();
                        assert!(sender.wait_closed(DEADLINE));
                        if sequence == 0 {
                            joining_tx.send(()).unwrap();
                            release
                                .lock()
                                .take()
                                .unwrap()
                                .recv_timeout(DEADLINE)
                                .unwrap();
                        }
                        Ok(())
                    })
                })
                .with_max_producers(NonZeroUsize::new(1).unwrap()),
            )
            .unwrap();
        let runtime = SubscriptionRuntime::with_host(
            RuntimeGeneration::new(1).unwrap(),
            crate::subscription::RtdLimits::standard().with_max_active(
                crate::subscription::RtdCapacity::bounded(NonZeroUsize::new(1).unwrap()),
            ),
            RtdSubscriptionHost::detached(),
            registration.finish(),
        );
        let server = runtime.register_test_server(1);
        let first = runtime
            .prepare(&source, RtdTopic::single("first").unwrap().borrowed())
            .unwrap();
        server
            .connect_transaction(TopicId(1), first.id())
            .unwrap()
            .commit()
            .unwrap();
        first.commit();
        let first_sender = sender_rx.recv_timeout(DEADLINE).unwrap();
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        server
            .disconnect_deferred(TopicId(1), Box::new(CleanupFinished(finished_tx)))
            .unwrap();
        joining_rx.recv_timeout(DEADLINE).unwrap();
        assert!(first_sender.is_closed());
        assert_eq!(runtime.services.active_quota.used(), 0);
        assert_eq!(runtime.disconnects.retained_source_count(), 1);

        let second = runtime
            .prepare(&source, RtdTopic::single("second").unwrap().borrowed())
            .unwrap();
        assert!(matches!(
            server.connect_transaction(TopicId(2), second.id()),
            Err(XllError::Overloaded)
        ));
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "rejected connection must not invoke source factory"
        );
        assert_eq!(runtime.services.active_quota.used(), 0);
        assert_eq!(runtime.disconnects.retained_source_count(), 1);
        release_tx.send(()).unwrap();
        finished_rx.recv_timeout(DEADLINE).unwrap();
        assert_eq!(runtime.disconnects.retained_source_count(), 0);

        server
            .connect_transaction(TopicId(2), second.id())
            .unwrap()
            .commit()
            .unwrap();
        second.commit();
        let second_sender = sender_rx.recv_timeout(DEADLINE).unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(runtime.disconnects.retained_source_count(), 1);
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        server
            .disconnect_deferred(TopicId(2), Box::new(CleanupFinished(finished_tx)))
            .unwrap();
        finished_rx.recv_timeout(DEADLINE).unwrap();
        assert!(second_sender.is_closed());
        assert_eq!(runtime.disconnects.retained_source_count(), 0);
        assert_eq!(runtime.services.active_quota.used(), 0);
        assert_eq!(runtime.services.queued_update_quota.used(), 0);
        runtime.close().unwrap();
    }

    #[test]
    fn synchronous_cleanup_paths_retain_capacity_through_the_source_barrier() {
        for path in ["rollback", "disconnect", "server", "runtime"] {
            let (sender_tx, sender_rx) = mpsc::sync_channel(1);
            let (joining_tx, joining_rx) = mpsc::sync_channel(1);
            let (release_tx, release_rx) = mpsc::sync_channel(1);
            let release = StdArc::new(Mutex::new(Some(release_rx)));
            let registration = SourceRegistration::new(RuntimeGeneration::new(1).unwrap());
            let source = registration
                .register(RtdChannelSource::new(
                    NonZeroUsize::new(8).unwrap(),
                    move |_| {
                        let sender_tx = sender_tx.clone();
                        let joining_tx = joining_tx.clone();
                        let release = StdArc::clone(&release);
                        Ok(move |sender: RtdSender<i32>| {
                            sender_tx.send(sender.clone()).unwrap();
                            assert!(sender.wait_closed(DEADLINE));
                            joining_tx.send(()).unwrap();
                            release
                                .lock()
                                .take()
                                .unwrap()
                                .recv_timeout(DEADLINE)
                                .unwrap();
                            Ok(())
                        })
                    },
                ))
                .unwrap();
            let runtime = StdArc::new(SubscriptionRuntime::with_host(
                RuntimeGeneration::new(1).unwrap(),
                crate::subscription::RtdLimits::standard().with_max_active(
                    crate::subscription::RtdCapacity::bounded(NonZeroUsize::new(1).unwrap()),
                ),
                RtdSubscriptionHost::detached(),
                registration.finish(),
            ));
            let server = runtime.register_test_server(1);
            let prepared = runtime
                .prepare(&source, RtdTopic::single("synchronous").unwrap().borrowed())
                .unwrap();
            let connection = server
                .connect_transaction(TopicId(1), prepared.id())
                .unwrap();
            prepared.commit();
            let connection = if path == "rollback" {
                Some(connection)
            } else {
                connection.commit().unwrap();
                None
            };
            let sender = sender_rx.recv_timeout(DEADLINE).unwrap();
            let closing_runtime = StdArc::clone(&runtime);
            let (finished_tx, finished_rx) = mpsc::sync_channel(1);
            let closing = std::thread::spawn(move || {
                let result = match path {
                    "rollback" => {
                        drop(connection);
                        Ok(())
                    }
                    "disconnect" => server.disconnect(TopicId(1)),
                    "server" => server.terminate(),
                    _ => closing_runtime.close(),
                };
                finished_tx.send(result).unwrap();
            });
            joining_rx.recv_timeout(DEADLINE).unwrap();
            assert!(sender.is_closed());
            assert_eq!(runtime.disconnects.retained_source_count(), 1, "{path}");
            assert!(
                finished_rx.try_recv().is_err(),
                "barrier must still wait: {path}"
            );
            if matches!(path, "rollback" | "disconnect") {
                assert!(matches!(
                    runtime.disconnects.reserve(),
                    Err(XllError::Overloaded)
                ));
                assert_eq!(runtime.services.active_quota.used(), 0);
            }
            release_tx.send(()).unwrap();
            finished_rx.recv_timeout(DEADLINE).unwrap().unwrap();
            closing.join().unwrap();
            assert_eq!(runtime.disconnects.retained_source_count(), 0, "{path}");
            assert_eq!(runtime.services.active_quota.used(), 0);
            runtime.close().unwrap();
        }
    }

    #[test]
    fn source_setup_error_and_panic_release_terminal_cleanup_capacity() {
        for panic in [false, true] {
            let calls = StdArc::new(std::sync::atomic::AtomicUsize::new(0));
            let called = StdArc::clone(&calls);
            let registration = SourceRegistration::new(RuntimeGeneration::new(1).unwrap());
            let source = registration
                .register(
                    RtdChannelSource::new(NonZeroUsize::new(8).unwrap(), move |_| {
                        if called.fetch_add(1, Ordering::Relaxed) == 0 {
                            assert!(!panic, "injected source setup panic");
                            return Err(XllError::Overloaded);
                        }
                        Ok(move |sender: RtdSender<i32>| {
                            assert!(sender.wait_closed(DEADLINE));
                            Ok(())
                        })
                    })
                    .with_max_producers(NonZeroUsize::new(1).unwrap()),
                )
                .unwrap();
            let runtime = SubscriptionRuntime::with_host(
                RuntimeGeneration::new(1).unwrap(),
                crate::subscription::RtdLimits::standard().with_max_active(
                    crate::subscription::RtdCapacity::bounded(NonZeroUsize::new(1).unwrap()),
                ),
                RtdSubscriptionHost::detached(),
                registration.finish(),
            );
            let server = runtime.register_test_server(1);
            let prepared = runtime
                .prepare(&source, RtdTopic::single("retry").unwrap().borrowed())
                .unwrap();
            let result = catch_no_unwind(AssertUnwindSafe(|| {
                server.connect_transaction(TopicId(1), prepared.id())
            }));
            if panic {
                assert!(result.is_err());
            } else {
                assert!(matches!(result, Ok(Err(XllError::Overloaded))));
            }
            assert_eq!(runtime.services.active_quota.used(), 0);
            assert_eq!(runtime.disconnects.retained_source_count(), 0);
            server
                .connect_transaction(TopicId(1), prepared.id())
                .unwrap()
                .commit()
                .unwrap();
            prepared.commit();
            server.disconnect(TopicId(1)).unwrap();
            assert_eq!(calls.load(Ordering::Relaxed), 2);
            assert_eq!(runtime.disconnects.retained_source_count(), 0);
            let result = runtime.close();
            assert_eq!(result.is_err(), panic);
        }
    }

    #[test]
    fn stop_preflight_failure_retains_worker_owner_and_is_terminal_for_close() {
        let (runtime, server, sender) = channel_runtime(RtdSubscriptionHost::detached());
        runtime
            .disconnects
            .fail_stop_preflight
            .store(true, Ordering::Relaxed);
        let close = || {
            assert!(matches!(runtime.close(), Err(XllError::Overloaded)));
        };
        close();
        assert!(sender.is_closed());
        assert_eq!(runtime.services.active_quota.used(), 0);
        assert_eq!(runtime.disconnects.retained_source_count(), 0);
        assert!(matches!(
            server.disconnect_deferred(TopicId(1), Box::new(())),
            Err(XllError::Closing)
        ));
        {
            let worker = runtime.disconnects.worker.lock();
            let DisconnectWorker::Running(queue, handle) = &*worker else {
                panic!("failed preflight must retain the worker's unique owner");
            };
            assert!(
                !handle.is_finished(),
                "unused join must not stop the worker"
            );
            assert!(!queue.ready.lock().stopping);
        }
        assert_eq!(runtime.disconnects.stop_attempts.load(Ordering::Relaxed), 1);
        // A failed close seals the runtime; both sequential and cross-thread
        // callers observe its retained error without attempting stop again.
        close();
        let waiter_runtime = StdArc::clone(&runtime);
        assert!(matches!(
            std::thread::spawn(move || waiter_runtime.close())
                .join()
                .unwrap(),
            Err(XllError::Overloaded)
        ));
        assert_eq!(runtime.disconnects.stop_attempts.load(Ordering::Relaxed), 1);
        assert!(matches!(
            runtime.termination_coordinator.state.lock().phase,
            super::super::server::ServerTerminationPhase::Failed
        ));
        // Explicit fixture cleanup discharges the retained worker owner. Real
        // teardown keeps the failed generation in its existing quarantine.
        runtime.disconnects.stop().unwrap();
    }

    #[test]
    fn deferred_disconnect_failures_reach_shutdown_after_the_sink_barrier() {
        for (fail_cancel, fail_disconnect) in [(true, false), (false, true)] {
            let sink = StdArc::new(Mutex::new(None));
            let disconnected = StdArc::new(AtomicBool::new(false));
            let registration = SourceRegistration::new(RuntimeGeneration::new(1).unwrap());
            let source = registration
                .register(FixtureSource {
                    sink: StdArc::clone(&sink),
                    disconnected: StdArc::clone(&disconnected),
                    fail_cancel,
                    fail_disconnect,
                })
                .unwrap();
            let runtime = SubscriptionRuntime::<RtdSubscriptionHost>::with_sources_for_internal(
                registration.finish(),
            );
            let server = runtime.register_test_server(1);
            let prepared = runtime
                .prepare(&source, RtdTopic::single("failure").unwrap().borrowed())
                .unwrap();
            server
                .connect_transaction(TopicId(1), prepared.id())
                .unwrap()
                .commit()
                .unwrap();
            prepared.commit();
            server
                .disconnect_deferred(TopicId(1), Box::new(()))
                .unwrap();
            let result = runtime.close();
            if fail_cancel {
                assert!(matches!(result, Err(XllError::Panic)));
            } else {
                assert!(matches!(result, Err(XllError::Overloaded)));
            }
            assert!(disconnected.load(Ordering::Acquire));
            assert!(sink.lock().is_none());
            assert_eq!(runtime.disconnects.retained_source_count(), 0);
            assert_eq!(runtime.services.active_quota.used(), 0);
        }
    }
}
