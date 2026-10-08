use super::*;
use crate::async_udf::{AsyncRuntime, AsyncTaskScope, CalculationEpoch, HandleScopedBuilder};
use crate::{AsyncPollerCount, AsyncTaskLimit, BuiltinAsyncExecutor, BuiltinExecutorConfig};
type ScopedRuntime = AsyncRuntime<BuiltinAsyncExecutor>;
fn builtin() -> BuiltinAsyncExecutor {
    BuiltinAsyncExecutor::new(
        BuiltinExecutorConfig::new().with_poller_count(AsyncPollerCount::new(1).unwrap()),
    )
}
use crate::execution::{
    CalculationId, CallTimer, UdfCompletionOutcome, UdfDeliveryOutcome, UdfErrorKind,
};
use crate::generation::RuntimeGeneration;
use crate::handle::{
    ExcelHandleObject, FormulaCaller, FormulaHandleService, FormulaRevisionKey, HandleLease,
    HandleTopicKey, PendingHandleLease,
};
use crate::input_identity::InputFingerprint;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use xlfn_sys::{XLOPER12BigData, XLOPER12BigDataHandle, XLOPER12Value, XLTYPE_BIG_DATA};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Ready,
    Error,
    PollPanic,
    DropPanic,
    Pending,
    BuildPanic,
}

struct Payload(Arc<AtomicUsize>);
impl ExcelHandleObject for Payload {}
impl Drop for Payload {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

struct Builder {
    pending: PendingHandleLease<Payload>,
    mode: Mode,
    polls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
    started: std::sync::mpsc::Sender<()>,
    reenter: Option<std::sync::Weak<ScopedRuntime>>,
}

struct Inner<'generation> {
    lease: Option<HandleLease<'generation, Payload>>,
    mode: Mode,
    polls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
    started: Option<std::sync::mpsc::Sender<()>>,
    reenter: Option<std::sync::Weak<ScopedRuntime>>,
}

impl HandleScopedBuilder<f64> for Builder {
    fn build<'generation>(
        self,
        scope: AsyncTaskScope<'generation>,
    ) -> Pin<Box<dyn Future<Output = XllResult<f64>> + Send + 'generation>> {
        assert_ne!(self.mode, Mode::BuildPanic, "injected scoped builder panic");
        Box::pin(Inner {
            lease: Some(self.pending.bind(scope)),
            mode: self.mode,
            polls: self.polls,
            drops: self.drops,
            started: Some(self.started),
            reenter: self.reenter,
        })
    }
}

impl Future for Inner<'_> {
    type Output = XllResult<f64>;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.polls.fetch_add(1, Ordering::Relaxed);
        assert_eq!(
            this.lease.as_ref().unwrap().0.load(Ordering::Relaxed),
            0,
            "the task must retain its raw object pin during evaluation"
        );
        if let Some(started) = this.started.take() {
            started.send(()).unwrap();
        }
        match this.mode {
            Mode::Ready | Mode::DropPanic => Poll::Ready(Ok(42.0)),
            Mode::Error => Poll::Ready(Err(XllError::Overloaded)),
            Mode::PollPanic => panic!("injected scoped poll panic"),
            Mode::Pending => Poll::Pending,
            Mode::BuildPanic => unreachable!(),
        }
    }
}

impl Drop for Inner<'_> {
    fn drop(&mut self) {
        drop(self.lease.take());
        self.drops.fetch_add(1, Ordering::Relaxed);
        if let Some(lifecycle) = self.reenter.take().and_then(|weak| weak.upgrade()) {
            assert!(
                lifecycle.advance_calculation(),
                "inner destruction may reenter the executor"
            );
        }
        assert_ne!(self.mode, Mode::DropPanic, "injected scoped drop panic");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Completion {
    Success,
    Error(UdfErrorKind),
    Cancelled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delivery {
    Delivered,
    Unobserved,
}

struct Observer {
    events: Arc<Mutex<Vec<(Completion, Delivery)>>>,
    payload_drops: Arc<AtomicUsize>,
    require_release: bool,
}
impl crate::execution::UdfLayerGuard for Observer {
    fn exit(self, outcome: &crate::execution::CallOutcome<'_>) {
        if self.require_release {
            assert_eq!(
                self.payload_drops.load(Ordering::Relaxed),
                1,
                "successful evaluation disposes the inner future before reporting completion"
            );
        }
        let completion = match outcome.completion {
            UdfCompletionOutcome::Success => Completion::Success,
            UdfCompletionOutcome::Error { kind, .. } => Completion::Error(kind),
            UdfCompletionOutcome::Cancelled => Completion::Cancelled,
        };
        let delivery = match outcome.delivery {
            UdfDeliveryOutcome::Delivered => Delivery::Delivered,
            UdfDeliveryOutcome::Unobserved => Delivery::Unobserved,
            ref unexpected => panic!("unexpected scoped delivery: {unexpected:?}"),
        };
        self.events.lock().push((completion, delivery));
    }
}

fn pending(
    handles: &FormulaHandleService,
    drops: &Arc<AtomicUsize>,
) -> PendingHandleLease<Payload> {
    let token = handles
        .prepare(
            HandleTopicKey::Formula(FormulaRevisionKey::new(
                FormulaCaller {
                    sheet_id: 1,
                    row: 0,
                    column: 0,
                },
                "SCOPED.TEST",
                InputFingerprint::from_bytes([0; 32]),
            )),
            || Ok(Payload(Arc::clone(drops))),
        )
        .unwrap()
        .into_token();
    let pending = crate::call::with_excel_call_scope_and_state(handles, |handles, scope| {
        handles
            .lookup::<Payload>(scope, &token)
            .unwrap()
            .into_pending(RuntimeGeneration::new(1).unwrap())
            .unwrap()
    });
    handles.terminate_all_topics();
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    pending
}

fn responder() -> ExcelAsyncResponder {
    let mut raw = XLOPER12 {
        value: XLOPER12Value {
            big_data: XLOPER12BigData {
                handle: XLOPER12BigDataHandle {
                    data: std::ptr::null_mut(),
                },
                byte_count: 0,
            },
        },
        xltype: XLTYPE_BIG_DATA,
    };
    // SAFETY: raw is a well-formed, live opaque async token, copied immediately.
    unsafe { ExcelAsyncResponder::from_raw("SCOPED.TEST", &mut raw).unwrap() }
}

fn observation(
    token: CancellationToken,
    events: Arc<Mutex<Vec<(Completion, Delivery)>>>,
    payload_drops: Arc<AtomicUsize>,
    require_release: bool,
) -> AsyncObservation<Observer> {
    let metadata = CallMetadata {
        udf_id: "SCOPED.TEST",
        excel_name: "SCOPED.TEST",
        call_id: CallId::new(1),
        calculation_id: CalculationId::new(1),
        started_at: std::time::SystemTime::UNIX_EPOCH,
        concurrent_calls: 1,
    };
    AsyncObservation::new(
        &metadata,
        CallTimer::start(),
        Some(Observer {
            events,
            payload_drops,
            require_release,
        }),
        false,
        token,
    )
}

fn run(mode: Mode, instrumented: bool, cancel_before_poll: bool, reenter: bool) {
    let _lock = crate::runtime::tests::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _callback = crate::test_callback::lock();
    crate::test_callback::install();
    crate::test_callback::reset();
    let handles = FormulaHandleService::new(1);
    let payload_drops = Arc::new(AtomicUsize::new(0));
    let polls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let lifecycle = Arc::new(ScopedRuntime::new());
    lifecycle.start(builtin(), AsyncTaskLimit::DEFAULT).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let (started, started_rx) = std::sync::mpsc::channel();
    let build = Builder {
        pending: pending(&handles, &payload_drops),
        mode,
        polls: Arc::clone(&polls),
        drops: Arc::clone(&drops),
        started,
        reenter: reenter.then(|| Arc::downgrade(&lifecycle)),
    };
    let (cancellation, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    if cancel_before_poll {
        let cancel = Arc::clone(&lifecycle);
        lifecycle.set_before_submit_hook(Some(Arc::new(move || {
            cancel.cancel_calculation(CalculationEpoch::INITIAL)
        })));
    }
    let reservation = lifecycle.reserve(CalculationEpoch::INITIAL).unwrap();
    let commit = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
        if instrumented {
            reservation.commit_handle_scoped(
                RuntimeGeneration::new(1).unwrap(),
                InstrumentedHandleTask {
                    build,
                    responder: responder(),
                    token,
                    observation: observation(
                        token,
                        Arc::clone(&events),
                        Arc::clone(&payload_drops),
                        !cancel_before_poll && mode != Mode::Pending && mode != Mode::BuildPanic,
                    ),
                    udf_id: "SCOPED.TEST",
                    _result: std::marker::PhantomData,
                },
                cancellation,
            );
        } else {
            reservation.commit_handle_scoped(
                RuntimeGeneration::new(1).unwrap(),
                UninstrumentedHandleTask {
                    build,
                    responder: responder(),
                    token,
                    udf_id: "SCOPED.TEST",
                    _result: std::marker::PhantomData,
                },
                cancellation,
            );
        }
    }));
    lifecycle.set_before_submit_hook(None);
    assert_eq!(commit.is_err(), mode == Mode::BuildPanic);
    if mode == Mode::Pending && !cancel_before_poll {
        started_rx.recv().unwrap();
        assert_eq!(payload_drops.load(Ordering::Relaxed), 0);
        lifecycle.cancel_calculation(CalculationEpoch::INITIAL);
    }
    {
        let shared = lifecycle.registry();
        let mut wait = shared.idle_lock.lock();
        while shared.active_tasks.load(Ordering::Acquire) != 0 {
            shared.idle.wait(&mut wait);
        }
    }
    assert!(lifecycle.close().unwrap().issues.is_empty());
    assert_eq!(payload_drops.load(Ordering::Relaxed), 1);
    assert_eq!(
        drops.load(Ordering::Relaxed),
        usize::from(mode != Mode::BuildPanic)
    );
    assert_eq!(
        crate::test_callback::async_return_calls(),
        1,
        "the responder delivers exactly once"
    );
    if cancel_before_poll {
        assert_eq!(polls.load(Ordering::Relaxed), 0);
    }
    if instrumented {
        let expected = if cancel_before_poll {
            (Completion::Cancelled, Delivery::Unobserved)
        } else {
            match mode {
                Mode::Ready => (Completion::Success, Delivery::Delivered),
                Mode::Error => (
                    Completion::Error(UdfErrorKind::Internal),
                    Delivery::Delivered,
                ),
                Mode::PollPanic | Mode::DropPanic => {
                    (Completion::Error(UdfErrorKind::Panic), Delivery::Delivered)
                }
                Mode::Pending => (Completion::Cancelled, Delivery::Unobserved),
                Mode::BuildPanic => (
                    Completion::Error(UdfErrorKind::Internal),
                    Delivery::Unobserved,
                ),
            }
        };
        assert_eq!(events.lock().as_slice(), &[expected]);
    }
    handles.seal().unwrap();
}

#[test]
fn miri_scoped_delivery_completion_disposes_inner_before_reporting() {
    for instrumented in [false, true] {
        for mode in [Mode::Ready, Mode::Error, Mode::PollPanic, Mode::DropPanic] {
            run(mode, instrumented, false, false);
        }
    }
}
#[test]
fn miri_scoped_delivery_builder_panic_releases_reservation_and_raw_pin() {
    for instrumented in [false, true] {
        run(Mode::BuildPanic, instrumented, false, false);
    }
}
#[test]
fn miri_scoped_delivery_cancellation_disposes_unpolled_and_polled_tasks_once() {
    for instrumented in [false, true] {
        for before in [false, true] {
            run(Mode::Pending, instrumented, before, false);
        }
        run(Mode::DropPanic, instrumented, true, false);
    }
}
#[test]
fn miri_scoped_delivery_inner_destruction_reenters_generation_rotation() {
    for instrumented in [false, true] {
        run(Mode::Ready, instrumented, false, true);
    }
}

#[test]
fn miri_external_executor_retains_scoped_handle_until_opaque_task_destruction() {
    type StoredTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
    struct ExternalExecutor(Arc<Mutex<Option<StoredTask>>>);
    impl crate::AsyncExecutor for ExternalExecutor {
        type Reservation = ();
        fn start(&self) -> XllResult<()> {
            Ok(())
        }
        fn reserve(&self) -> XllResult<()> {
            Ok(())
        }
        fn submit<F>(&self, (): (), task: crate::AsyncTask<F>)
        where
            F: Future<Output = ()> + Send + 'static,
        {
            *self.0.lock() = Some(Box::pin(task));
        }
        fn shutdown(&self) -> XllResult<()> {
            assert!(self.0.lock().is_none());
            Ok(())
        }
    }
    let _test = crate::runtime::tests::TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _callback = crate::test_callback::lock();
    crate::test_callback::install();
    crate::test_callback::reset();
    let handles = FormulaHandleService::new(1);
    let payload_drops = Arc::new(AtomicUsize::new(0));
    let polls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let held = Arc::new(Mutex::new(None));
    let lifecycle = Arc::new(AsyncRuntime::new());
    lifecycle
        .start(ExternalExecutor(Arc::clone(&held)), AsyncTaskLimit::DEFAULT)
        .unwrap();
    let (started, _) = std::sync::mpsc::channel();
    let build = Builder {
        pending: pending(&handles, &payload_drops),
        mode: Mode::Pending,
        polls: Arc::clone(&polls),
        drops: Arc::clone(&drops),
        started,
        reenter: None,
    };
    let (cancellation, token) = CancellationSource::new(CancellationGuarantee::CalculationScoped);
    lifecycle
        .reserve(CalculationEpoch::INITIAL)
        .unwrap()
        .commit_handle_scoped(
            RuntimeGeneration::new(1).unwrap(),
            UninstrumentedHandleTask {
                build,
                responder: responder(),
                token,
                udf_id: "EXTERNAL.SCOPED",
                _result: std::marker::PhantomData,
            },
            cancellation,
        );
    assert_eq!(polls.load(Ordering::Relaxed), 0);
    assert_eq!(payload_drops.load(Ordering::Relaxed), 0);
    let (closed_tx, closed_rx) = std::sync::mpsc::channel();
    let closing = Arc::clone(&lifecycle);
    let closer = std::thread::spawn(move || {
        closed_tx.send(closing.close().is_ok()).unwrap();
    });
    assert!(lifecycle.wait_for_closing(std::time::Duration::from_secs(1)));
    assert!(
        closed_rx
            .recv_timeout(std::time::Duration::from_millis(10))
            .is_err()
    );
    assert_eq!(lifecycle.registry().active_tasks.load(Ordering::Acquire), 1);
    let task = held.lock().take();
    drop(task);
    assert!(
        closed_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap()
    );
    closer.join().unwrap();
    assert_eq!(polls.load(Ordering::Relaxed), 0);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(payload_drops.load(Ordering::Relaxed), 1);
    assert_eq!(crate::test_callback::async_return_calls(), 1);
}
