use super::*;
use crate::excel_rtd::RtdNotifier;
use crate::rtd::test_support::{TestNotifierState, TestNotifyOutcome};
use crate::subscription::tests::connected_sink;
use crate::subscription::{RefreshOutcome, StoredRtdValue};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone, Copy, Default, PartialEq)]
enum Fault {
    #[default]
    None,
    CreateError,
    AccessError,
    AccessPanic,
    FillError,
    FillPanic,
    UnlockError,
    UnlockPanic,
}

#[derive(Default)]
struct ApiState {
    fault: Fault,
    locked: bool,
    created: usize,
    destroyed: usize,
    unlocks: usize,
    retained: usize,
    events: Vec<&'static str>,
    output: Option<Rc<RefCell<OutputState>>>,
    require_empty_output_at_destroy: bool,
}
thread_local! {
    static API: RefCell<ApiState> = RefCell::new(ApiState::default());
}

struct MockApi;
impl ArrayApi for MockApi {
    type Array = usize;
    type Data = usize;
    fn create(count: u32) -> Result<usize, i32> {
        assert_eq!(count, 1);
        API.with(|state| {
            let mut state = state.borrow_mut();
            state.events.push("create");
            if state.fault == Fault::CreateError {
                return Err(-1);
            }
            state.created += 1;
            Ok(1)
        })
    }
    fn access(array: usize) -> Result<usize, i32> {
        assert_eq!(array, 1);
        API.with(|state| {
            let mut state = state.borrow_mut();
            state.events.push("access");
            if state.fault == Fault::AccessError {
                return Err(-2);
            }
            state.locked = true;
            assert!(
                state.fault != Fault::AccessPanic,
                "indeterminate successful access"
            );
            Ok(array)
        })
    }
    fn fill(data: usize, updates: &[RtdUpdate]) -> Result<(), i32> {
        assert_eq!(data, 1);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].value, StoredRtdValue::Integer(42));
        API.with(|state| {
            let mut state = state.borrow_mut();
            assert!(state.locked);
            state.events.push("partial fill");
            assert!(state.fault != Fault::FillPanic, "after partial fill");
            if state.fault == Fault::FillError {
                return Err(-3);
            }
            Ok(())
        })
    }
    fn unaccess(array: usize) -> Result<(), i32> {
        assert_eq!(array, 1);
        API.with(|state| {
            let mut state = state.borrow_mut();
            assert!(state.locked);
            state.unlocks += 1;
            state.events.push("unlock");
            assert!(state.fault != Fault::UnlockPanic, "indeterminate unlock");
            if state.fault == Fault::UnlockError {
                return Err(-4);
            }
            state.locked = false;
            Ok(())
        })
    }
    fn destroy(array: usize) {
        assert_eq!(array, 1);
        API.with(|state| {
            let mut state = state.borrow_mut();
            assert!(
                !state.locked,
                "never destroy while access may still be held"
            );
            assert_eq!(state.destroyed, 0, "one allocation is destroyed once");
            if state.require_empty_output_at_destroy {
                let output = state.output.as_ref().unwrap().borrow();
                assert_eq!((output.count, output.array), (0, None));
            }
            state.destroyed += 1;
            state.events.push("destroy");
        });
    }
    fn retained(array: usize, status: i32) {
        assert_eq!((array, status), (1, -4));
        API.with(|state| {
            let mut state = state.borrow_mut();
            state.retained += 1;
            state.events.push("retain");
        });
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
enum WriteFault {
    #[default]
    None,
    Count,
    Pointer,
}
#[derive(Default)]
struct OutputState {
    count: i32,
    array: Option<usize>,
    resets: usize,
    fault: WriteFault,
}
struct MockOutput(Rc<RefCell<OutputState>>);
impl OutputSlots<MockApi> for MockOutput {
    fn write_count(&mut self, count: i32) -> Result<(), i32> {
        let mut output = self.0.borrow_mut();
        output.count = count;
        if output.fault == WriteFault::Count {
            Err(-5)
        } else {
            Ok(())
        }
    }
    fn write_array(&mut self, array: Option<usize>) -> Result<(), i32> {
        let mut output = self.0.borrow_mut();
        output.array = array;
        if output.fault == WriteFault::Pointer {
            Err(-6)
        } else {
            Ok(())
        }
    }
    fn reset(&mut self) {
        let mut output = self.0.borrow_mut();
        output.count = 0;
        output.array = None;
        output.resets += 1;
        API.with(|state| state.borrow_mut().events.push("reset outputs"));
    }
}

fn reset_api(fault: Fault) {
    API.with(|state| {
        *state.borrow_mut() = ApiState {
            fault,
            ..ApiState::default()
        }
    });
}
fn outputs(fault: WriteFault) -> Rc<RefCell<OutputState>> {
    let output = Rc::new(RefCell::new(OutputState {
        fault,
        ..OutputState::default()
    }));
    API.with(|state| state.borrow_mut().output = Some(Rc::clone(&output)));
    output
}

#[test]
fn miri_marshal_failures_restore_exact_batch_and_obey_array_lock_ownership() {
    for fault in [
        Fault::CreateError,
        Fault::AccessError,
        Fault::AccessPanic,
        Fault::FillError,
        Fault::FillPanic,
        Fault::UnlockError,
        Fault::UnlockPanic,
    ] {
        reset_api(fault);
        let (_runtime, server, sink) = connected_sink::<i32>(None, "marshal-failure");
        sink.publish(42).unwrap();
        let batch = server.begin_refresh().unwrap();
        let failed = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
            RefreshPublication::<_, MockApi>::prepare(batch)
        }));
        if matches!(
            fault,
            Fault::AccessPanic | Fault::FillPanic | Fault::UnlockPanic
        ) {
            assert!(failed.is_err());
        } else {
            assert!(matches!(failed, Ok(Err(_))));
        }
        API.with(|state| {
            let state = state.borrow();
            match fault {
                Fault::CreateError => {
                    assert_eq!((state.created, state.destroyed, state.unlocks), (0, 0, 0))
                }
                Fault::AccessError => assert_eq!((state.destroyed, state.unlocks), (1, 0)),
                Fault::AccessPanic => assert_eq!((state.destroyed, state.unlocks), (0, 0)),
                Fault::FillError | Fault::FillPanic => {
                    assert_eq!((state.destroyed, state.unlocks), (1, 1))
                }
                Fault::UnlockError => {
                    assert_eq!((state.destroyed, state.unlocks, state.retained), (0, 1, 1))
                }
                Fault::UnlockPanic => assert_eq!((state.destroyed, state.unlocks), (0, 1)),
                Fault::None => unreachable!(),
            }
        });
        let retry = server.begin_refresh().unwrap();
        assert_eq!(retry.updates().len(), 1);
        assert_eq!(retry.updates()[0].value, StoredRtdValue::Integer(42));
        retry.complete(RefreshOutcome::Delivered).unwrap();
    }
}

#[test]
fn miri_partial_output_errors_and_panics_reset_before_array_destroy_and_batch_restore() {
    for (fault, stage) in [
        (WriteFault::Count, None),
        (WriteFault::Pointer, None),
        (WriteFault::None, Some(HandoffStage::CountWritten)),
        (WriteFault::None, Some(HandoffStage::PointerWritten)),
    ] {
        reset_api(Fault::None);
        let output = outputs(fault);
        API.with(|state| state.borrow_mut().require_empty_output_at_destroy = true);
        let (_runtime, server, sink) = connected_sink::<i32>(None, "partial-output");
        sink.publish(42).unwrap();
        let publication =
            RefreshPublication::<_, MockApi>::prepare(server.begin_refresh().unwrap())
                .unwrap_or_else(|_| panic!("mock marshal succeeds"));
        let result = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
            publication.handoff_with(MockOutput(Rc::clone(&output)), |at| {
                assert!(Some(at) != stage, "after partial output write");
            })
        }));
        if stage.is_some() {
            assert!(result.is_err());
        } else {
            assert!(matches!(result, Ok(Err(_))));
        }
        assert_eq!(
            (
                output.borrow().count,
                output.borrow().array,
                output.borrow().resets
            ),
            (0, None, 1)
        );
        API.with(|state| {
            let state = state.borrow();
            assert_eq!(state.destroyed, 1);
            let reset = state
                .events
                .iter()
                .position(|event| *event == "reset outputs")
                .unwrap();
            let destroy = state
                .events
                .iter()
                .position(|event| *event == "destroy")
                .unwrap();
            assert!(reset < destroy);
        });
        let retry = server.begin_refresh().unwrap();
        assert_eq!(retry.updates()[0].value, StoredRtdValue::Integer(42));
        retry.complete(RefreshOutcome::Delivered).unwrap();
    }
}

#[test]
fn miri_unpublished_array_is_destroyed_before_rollback_notification_reenters() {
    reset_api(Fault::None);
    let (runtime, server, sink) = connected_sink::<i32>(None, "rollback-reentry");
    let notifier = Arc::new(TestNotifierState::new());
    server
        .attach_update_notifier(RtdNotifier::for_test(Arc::clone(&notifier)))
        .unwrap();
    sink.publish(42).unwrap();
    let publication = RefreshPublication::<_, MockApi>::prepare(server.begin_refresh().unwrap())
        .unwrap_or_else(|_| panic!("marshal succeeds"));
    let reentrant = server;
    let callback_runtime = Arc::clone(&runtime);
    *notifier.callback.lock() = Some(Box::new(move || {
        // Keep the outer Arc too: dropping the last runtime owner inside this
        // callback would correctly wait on the still-active outer operation.
        let _lifetime = callback_runtime;
        API.with(|state| assert_eq!(state.borrow().destroyed, 1));
        let retry = reentrant.begin_refresh().unwrap();
        assert_eq!(retry.updates()[0].value, StoredRtdValue::Integer(42));
        retry.complete(RefreshOutcome::Delivered).unwrap();
    }));
    drop(publication);
    assert_eq!(server.pending_update_count(), 0);
    assert!(
        notifier.callback.lock().is_none(),
        "rollback drove the reentrant notification"
    );
}

#[test]
fn miri_after_handoff_completion_failure_keeps_the_transferred_array() {
    for panic_notification in [false, true] {
        reset_api(Fault::None);
        let output = outputs(WriteFault::None);
        let (_runtime, server, sink) = connected_sink::<i32>(None, "completion-failure");
        let notifier = Arc::new(TestNotifierState::new());
        server
            .attach_update_notifier(RtdNotifier::for_test(Arc::clone(&notifier)))
            .unwrap();
        sink.publish(42).unwrap();
        let publication =
            RefreshPublication::<_, MockApi>::prepare(server.begin_refresh().unwrap())
                .unwrap_or_else(|_| panic!("marshal succeeds"));
        if panic_notification {
            sink.publish(43).unwrap();
            notifier.outcomes.lock().push_back(TestNotifyOutcome::Panic);
        } else {
            server
                .test_server()
                .publish
                .refresh
                .lock()
                .next_notification_ticket = u64::MAX;
        }
        let handoff = publication
            .handoff_with(MockOutput(Rc::clone(&output)), |_| {})
            .unwrap();
        assert!(handoff.completion().is_err());
        assert_eq!(
            (
                output.borrow().count,
                output.borrow().array,
                output.borrow().resets
            ),
            (1, Some(1), 0)
        );
        API.with(|state| assert_eq!(state.borrow().destroyed, 0));
        // The mock host, not the publication transaction, owns destruction now.
        MockApi::destroy(1);
        server
            .test_server()
            .publish
            .refresh
            .lock()
            .next_notification_ticket = 1;
        let retry = server.begin_refresh().unwrap();
        assert_eq!(
            retry.updates()[0].value,
            StoredRtdValue::Integer(if panic_notification { 43 } else { 42 })
        );
        retry.complete(RefreshOutcome::Delivered).unwrap();
    }
}

#[test]
fn miri_panic_after_commit_does_not_reset_outputs_or_restore_delivered_batch() {
    reset_api(Fault::None);
    let output = outputs(WriteFault::None);
    let (_runtime, server, sink) = connected_sink::<i32>(None, "committed-panic");
    sink.publish(42).unwrap();
    let publication = RefreshPublication::<_, MockApi>::prepare(server.begin_refresh().unwrap())
        .unwrap_or_else(|_| panic!("marshal succeeds"));
    let failure = crate::panic_boundary::catch_no_unwind(AssertUnwindSafe(|| {
        publication.handoff_with(MockOutput(Rc::clone(&output)), |stage| {
            assert!(stage != HandoffStage::Committed, "after local handoff");
        })
    }));
    assert!(failure.is_err());
    assert_eq!(
        (
            output.borrow().count,
            output.borrow().array,
            output.borrow().resets
        ),
        (1, Some(1), 0)
    );
    API.with(|state| assert_eq!(state.borrow().destroyed, 0));
    assert_eq!(server.pending_update_count(), 0);
    MockApi::destroy(1);
}

#[test]
#[allow(
    clippy::mem_forget,
    reason = "exercise safe forgotten publication without completing it"
)]
#[cfg_attr(
    miri,
    ignore = "intentionally leaks the prepared batch and its runtime issuer"
)]
fn forgotten_prepared_publication_never_hands_off_or_certifies_delivery() {
    reset_api(Fault::None);
    let (runtime, server, sink) = connected_sink::<i32>(None, "forgotten-publication");
    sink.publish(42).unwrap();
    let publication = RefreshPublication::<_, MockApi>::prepare(server.begin_refresh().unwrap())
        .unwrap_or_else(|_| panic!("marshal succeeds"));
    std::mem::forget(publication);
    API.with(|state| assert_eq!(state.borrow().destroyed, 0));
    assert_eq!(server.pending_update_count(), 1);
    std::mem::forget(runtime);
}

struct PointerApi;
impl ArrayApi for PointerApi {
    type Array = std::ptr::NonNull<u8>;
    type Data = usize;
    fn create(count: u32) -> Result<Self::Array, i32> {
        MockApi::create(count).map(|_| std::ptr::NonNull::dangling())
    }
    fn access(_: Self::Array) -> Result<Self::Data, i32> {
        MockApi::access(1)
    }
    fn fill(data: Self::Data, updates: &[RtdUpdate]) -> Result<(), i32> {
        MockApi::fill(data, updates)
    }
    fn unaccess(_: Self::Array) -> Result<(), i32> {
        MockApi::unaccess(1)
    }
    fn destroy(_: Self::Array) {
        MockApi::destroy(1);
    }
    fn retained(_: Self::Array, status: i32) {
        MockApi::retained(1, status);
    }
}

/// The test callback runs synchronously during handoff completion; both stack
/// locations remain alive until it returns and are not borrowed as &mut.
struct ReentrantOutputRead {
    count: *mut i32,
    array: *mut *mut u8,
}
#[allow(
    unsafe_code,
    reason = "test-only witness for synchronous ABI output reentry"
)]
// SAFETY: the callback is taken synchronously before this test's output storage
// ends, and the test asserts it was consumed before leaving that storage.
unsafe impl Send for ReentrantOutputRead {}
impl ReentrantOutputRead {
    #[allow(
        unsafe_code,
        reason = "read live raw ABI output locations during notifier reentry"
    )]
    fn inspect(self) {
        // SAFETY: the surrounding test retains both stack outputs and invokes
        // this callback synchronously after local handoff, before they expire.
        unsafe {
            assert_eq!(*self.count, 1);
        }
        // SAFETY: the same synchronous callback retains this initialized output.
        unsafe {
            assert_eq!(*self.array, std::ptr::NonNull::<u8>::dangling().as_ptr());
        }
    }
}

#[test]
#[allow(
    unsafe_code,
    reason = "exercise the shared raw ABI output owner under notifier reentry"
)]
fn miri_notification_reentry_can_read_the_handed_off_raw_outputs() {
    reset_api(Fault::None);
    let (_runtime, server, sink) = connected_sink::<i32>(None, "raw-output-alias");
    let notifier = Arc::new(TestNotifierState::new());
    server
        .attach_update_notifier(RtdNotifier::for_test(Arc::clone(&notifier)))
        .unwrap();
    sink.publish(42).unwrap();
    let publication = RefreshPublication::<_, PointerApi>::prepare(server.begin_refresh().unwrap())
        .unwrap_or_else(|_| panic!("marshal succeeds"));
    // A later update causes completion of the first batch to notify again.
    sink.publish(43).unwrap();
    let mut count = 0;
    let mut array = std::ptr::null_mut::<u8>();
    let count_pointer = std::ptr::addr_of_mut!(count);
    let array_pointer = std::ptr::addr_of_mut!(array);
    let witness = ReentrantOutputRead {
        count: count_pointer,
        array: array_pointer,
    };
    *notifier.callback.lock() = Some(Box::new(move || witness.inspect()));
    // Also remove the raw-pointer witness on an unexpected test failure before
    // the output stack storage ends; it must never survive into later cleanup.
    let _clear = scopeguard::guard(Arc::clone(&notifier), |notifier| {
        notifier.callback.lock().take();
    });
    // SAFETY: writable non-overlapping output locations remain live for this
    // handoff and the synchronous notification callback that reads them.
    let output = unsafe { RawOutputSlots::<u8>::initialize(count_pointer, array_pointer) }.unwrap();
    publication
        .handoff_with(output, |_| {})
        .unwrap()
        .completion()
        .unwrap();
    assert!(notifier.callback.lock().is_none());
    assert_eq!(count, 1);
    assert_eq!(array, std::ptr::NonNull::<u8>::dangling().as_ptr());
    API.with(|state| assert_eq!(state.borrow().destroyed, 0));
    // Simulate the host releasing the array after receiving ownership.
    PointerApi::destroy(std::ptr::NonNull::dangling());
    let remaining = server.begin_refresh().unwrap();
    assert_eq!(remaining.updates()[0].value, StoredRtdValue::Integer(43));
    remaining.complete(RefreshOutcome::Delivered).unwrap();
}
