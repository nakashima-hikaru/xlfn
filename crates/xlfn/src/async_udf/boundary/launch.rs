//! One framework launch owns its calculation, cancellation and copied response
//! handle through preparation and reservation. No parent survives a transition.
use super::*;
use crate::async_udf::runtime::RuntimeCalculationSnapshot;
use crate::async_udf::runtime::RuntimeReservation;
#[cfg(feature = "handles")]
use crate::async_udf::task_scope::{AsyncTaskScope, HandleScopedTaskBuilder};
use crate::cancellation::TaskCancellationOwner;
use crate::panic_boundary::catch_udf_no_unwind;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

// Capturing through the admitted guard fixes the issuer before layer callbacks
// run, without allocating cancellation state until those callbacks succeed.
pub(super) struct CapturedLaunch<'guard, 'call, A: crate::Addin> {
    guard: &'guard crate::runtime::AsyncCallGuard<'call, A>,
    calculation: RuntimeCalculationSnapshot<'static, A::AsyncExecutor>,
}

impl<'guard, 'call, A: crate::Addin> CapturedLaunch<'guard, 'call, A> {
    pub(super) fn capture(guard: &'guard crate::runtime::AsyncCallGuard<'call, A>) -> Self {
        Self {
            guard,
            calculation: guard.async_runtime().snapshot_calculation(),
        }
    }

    pub(super) fn calculation_id(&self) -> crate::execution::CalculationId {
        self.calculation.epoch().into()
    }

    pub(super) fn into_launch(self) -> LaunchOwner<'guard, 'call, A> {
        let (cancellation, token) = TaskCancellationOwner::new(CancellationGuarantee::BestEffort);
        LaunchOwner {
            guard: self.guard,
            calculation: self.calculation,
            cancellation,
            token,
        }
    }
}

pub(super) struct LaunchOwner<'guard, 'call, A: crate::Addin> {
    guard: &'guard crate::runtime::AsyncCallGuard<'call, A>,
    calculation: RuntimeCalculationSnapshot<'static, A::AsyncExecutor>,
    cancellation: TaskCancellationOwner,
    token: CancellationToken,
}

pub(super) struct PreparedLaunch<'guard, 'call, A: crate::Addin, B, O: CompletionObserver> {
    owner: LaunchOwner<'guard, 'call, A>,
    capture: CaptureOwner<B, O>,
}

pub(super) struct ReservedLaunch<'guard, 'call, A: crate::Addin, B, O: CompletionObserver> {
    _guard: std::marker::PhantomData<&'guard crate::runtime::AsyncCallGuard<'call, A>>,
    reservation: RuntimeReservation<'static, A::AsyncExecutor>,
    cancellation: TaskCancellationOwner,
    // Release reservation and admission before capture cleanup can reenter.
    capture: CaptureOwner<B, O>,
}

struct CaptureParts<B, O: CompletionObserver> {
    body: B,
    delivery: PendingDelivery<O>,
}

impl<B, O: CompletionObserver> Rollback for CaptureParts<B, O> {
    fn rollback(self) {
        let Self { body, delivery } = self;
        // User captures, including raw pins, release before delivery and layer exit.
        let panic_message = catch_udf_no_unwind(AssertUnwindSafe(|| drop(body))).err();
        delivery.fallback(panic_message);
    }
}

/// Holds the user captures and response together until the next stage takes them.
struct CaptureOwner<B, O: CompletionObserver>(RollbackGuard<CaptureParts<B, O>>);

impl<B, O: CompletionObserver> CaptureOwner<B, O> {
    fn new(body: B, delivery: PendingDelivery<O>) -> Self {
        Self(RollbackGuard::new(CaptureParts { body, delivery }))
    }

    fn into_parts(self) -> (B, PendingDelivery<O>) {
        let CaptureParts { body, delivery } = self.0.complete();
        (body, delivery)
    }

    fn fail(self, error: XllError) {
        let (body, delivery) = self.into_parts();
        // Reservation failure remains the completion cause even if destroying
        // the rejected user's captures also panics, matching launch policy.
        let _ = catch_no_unwind(AssertUnwindSafe(|| drop(body)));
        delivery.fail(error);
    }
}

pub(super) struct Plain<F>(F);
#[cfg(feature = "handles")]
pub(super) struct Scoped<B> {
    build: B,
    generation: crate::generation::RuntimeGeneration,
}

/// The only distinction between the launch paths is the task body produced by
/// user preparation and how that body is turned into a reserved task.
pub(super) trait AsyncStart<A: crate::Addin> {
    type Body;

    fn start(
        self,
        call: &crate::runtime::CallGuard<'_, A>,
        lease: crate::generation::ExecutionLease<A>,
        token: CancellationToken,
    ) -> XllResult<Self::Body>;

    fn commit<O: CompletionObserver + Send + 'static>(
        reservation: ReservedLaunch<'_, '_, A, Self::Body, O>,
    );
}

pub(super) struct PlainStart<S>(pub(super) S);

impl<A, S, F, T> AsyncStart<A> for PlainStart<S>
where
    A: crate::Addin,
    S: FnOnce(
        &crate::runtime::CallGuard<'_, A>,
        crate::generation::ExecutionLease<A>,
        CancellationToken,
    ) -> XllResult<F>,
    F: Future<Output = XllResult<T>> + Send + 'static,
    T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
{
    type Body = Plain<F>;

    fn start(
        self,
        call: &crate::runtime::CallGuard<'_, A>,
        lease: crate::generation::ExecutionLease<A>,
        token: CancellationToken,
    ) -> XllResult<Self::Body> {
        (self.0)(call, lease, token).map(Plain)
    }

    fn commit<O: CompletionObserver + Send + 'static>(
        reservation: ReservedLaunch<'_, '_, A, Self::Body, O>,
    ) {
        reservation.commit();
    }
}

#[cfg(feature = "handles")]
pub(super) struct ScopedStart<S, T>(S, std::marker::PhantomData<fn() -> T>);

#[cfg(feature = "handles")]
impl<S, T> ScopedStart<S, T> {
    pub(super) fn new(start: S) -> Self {
        Self(start, std::marker::PhantomData)
    }
}

#[cfg(feature = "handles")]
impl<A, S, B, T> AsyncStart<A> for ScopedStart<S, T>
where
    A: crate::Addin,
    S: FnOnce(
        &crate::runtime::CallGuard<'_, A>,
        crate::generation::ExecutionLease<A>,
        CancellationToken,
    ) -> XllResult<B>,
    B: HandleScopedBuilder<T> + Send + 'static,
    T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
{
    type Body = Scoped<B>;

    fn start(
        self,
        call: &crate::runtime::CallGuard<'_, A>,
        lease: crate::generation::ExecutionLease<A>,
        token: CancellationToken,
    ) -> XllResult<Self::Body> {
        let generation = lease.generation();
        (self.0)(call, lease, token).map(|build| Scoped { build, generation })
    }

    fn commit<O: CompletionObserver + Send + 'static>(
        reservation: ReservedLaunch<'_, '_, A, Self::Body, O>,
    ) {
        reservation.commit::<T>();
    }
}

impl<'guard, 'call, A: crate::Addin> LaunchOwner<'guard, 'call, A> {
    pub(super) fn token(&self) -> CancellationToken {
        self.token
    }

    pub(super) fn calculation_id(&self) -> crate::execution::CalculationId {
        self.calculation.epoch().into()
    }

    pub(super) fn prepare<S: AsyncStart<A>, O: CompletionObserver>(
        self,
        start: S,
        responder: ExcelAsyncResponder,
        observation: O,
        diagnostics: Option<AsyncDiagnosticContext>,
    ) -> Option<PreparedLaunch<'guard, 'call, A, S::Body, O>> {
        // Bind the copied responder before any user preparation can run.
        let delivery =
            PendingDelivery::new_with_diagnostics(responder, self.token, observation, diagnostics);
        let body = catch_udf_no_unwind(AssertUnwindSafe(|| {
            self.calculation.preflight()?;
            let lease = self.guard.try_execution_lease()?;
            start.start(self.guard, lease, self.token)
        }));
        match body {
            Ok(Ok(body)) => Some(PreparedLaunch {
                owner: self,
                capture: CaptureOwner::new(body, delivery),
            }),
            Ok(Err(error)) => {
                delivery.fail(error);
                None
            }
            Err(message) => {
                delivery.fail_panic(message);
                None
            }
        }
    }
}

impl<'guard, 'call, A: crate::Addin, B, O: CompletionObserver>
    PreparedLaunch<'guard, 'call, A, B, O>
{
    pub(super) fn reserve(self) -> Option<ReservedLaunch<'guard, 'call, A, B, O>> {
        let Self { owner, capture } = self;
        match owner.calculation.reserve() {
            Ok(reservation) => Some(ReservedLaunch {
                _guard: std::marker::PhantomData,
                reservation,
                cancellation: owner.cancellation,
                capture,
            }),
            Err(error) => {
                capture.fail(error);
                None
            }
        }
    }
}

impl<A: crate::Addin, F, T, O: CompletionObserver + Send + 'static>
    ReservedLaunch<'_, '_, A, Plain<F>, O>
where
    F: Future<Output = XllResult<T>> + Send + 'static,
    T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
{
    pub(super) fn commit(self) {
        let (Plain(future), delivery) = self.capture.into_parts();
        self.reservation
            .commit_owned(delivery.task(future), self.cancellation);
    }
}

#[cfg(feature = "handles")]
impl<A: crate::Addin, B, O: CompletionObserver + Send + 'static>
    ReservedLaunch<'_, '_, A, Scoped<B>, O>
{
    pub(super) fn commit<T>(self)
    where
        B: HandleScopedBuilder<T> + Send + 'static,
        T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
    {
        let (Scoped { build, generation }, delivery) = self.capture.into_parts();
        let task = UnbuiltScopedTask::<B, O, T>::with_delivery(build, delivery);
        let future = async move {
            let pins = crate::handle::TaskPins::new();
            let scope = AsyncTaskScope::new(generation, &pins);
            let (future, delivery) = task.build_task(scope);
            UnbuiltScopedTask::<B, O, T>::deliver(delivery, future).await;
        };
        self.reservation.commit_owned(future, self.cancellation);
    }
}

#[cfg(feature = "handles")]
pub(super) struct UnbuiltScopedTask<B, O: CompletionObserver, T> {
    capture: CaptureOwner<B, O>,
    _result: std::marker::PhantomData<fn() -> T>,
}

#[cfg(feature = "handles")]
impl<B, O: CompletionObserver, T> UnbuiltScopedTask<B, O, T> {
    #[cfg(all(test, feature = "async-builtin"))]
    pub(super) fn new(
        builder: B,
        responder: ExcelAsyncResponder,
        token: CancellationToken,
        observation: O,
    ) -> Self {
        Self::with_delivery(builder, PendingDelivery::new(responder, token, observation))
    }

    fn with_delivery(builder: B, delivery: PendingDelivery<O>) -> Self {
        Self {
            capture: CaptureOwner::new(builder, delivery),
            _result: std::marker::PhantomData,
        }
    }
}

#[cfg(feature = "handles")]
struct PanicMessageFuture<T>(Option<String>, std::marker::PhantomData<fn() -> T>);

#[cfg(feature = "handles")]
impl<T> Future for PanicMessageFuture<T> {
    type Output = XllResult<T>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let message = self
            .0
            .take()
            .unwrap_or_else(|| "UDF panicked with a non-string payload".to_owned());
        std::panic::panic_any(message);
    }
}

#[cfg(feature = "handles")]
impl<B, O, T> HandleScopedTaskBuilder for UnbuiltScopedTask<B, O, T>
where
    B: HandleScopedBuilder<T> + 'static,
    O: CompletionObserver + Send + 'static,
    T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
{
    type Output = XllResult<T>;
    type Delivery = PendingDelivery<O>;

    #[allow(unsafe_code, reason = "Launch exclusively owns the scoped evaluation")]
    fn build_task<'generation>(
        self,
        scope: AsyncTaskScope<'generation>,
    ) -> (
        impl Future<Output = Self::Output> + Send + 'generation,
        Self::Delivery,
    ) {
        let (builder, delivery) = self.capture.into_parts();
        let future = match catch_udf_no_unwind(AssertUnwindSafe(|| builder.build(scope))) {
            Ok(future) => futures_util::future::Either::Left(future),
            Err(message) => futures_util::future::Either::Right(PanicMessageFuture(
                Some(message),
                std::marker::PhantomData,
            )),
        };
        // SAFETY: the short-lived scope is created for this single builder.
        // Only the returned evaluation can access its projections. The
        // delivery capability and XllResult<T> cannot borrow the scope.
        let future = unsafe { crate::async_udf::task_scope::with_pin_cleanup(scope, future) };
        (future, delivery)
    }

    fn deliver<'generation>(
        delivery: Self::Delivery,
        future: impl Future<Output = Self::Output> + Send + 'generation,
    ) -> impl Future<Output = ()> + Send + 'generation {
        delivery.task(future)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use xlfn_sys::{XLOPER12BigData, XLOPER12BigDataHandle, XLOPER12Value, XLTYPE_BIG_DATA};

    struct Captures<'a> {
        drops: &'a Cell<usize>,
        panic: bool,
    }
    impl Drop for Captures<'_> {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
            assert!(!self.panic, "injected prepared capture destructor panic");
        }
    }

    #[derive(Clone, Copy)]
    enum Failure {
        Internal,
        Panic,
        Overloaded,
    }
    struct Observation<'a> {
        drops: &'a Cell<usize>,
        completions: &'a Cell<usize>,
        expected: Failure,
    }
    impl CompletionObserver for Observation<'_> {
        fn finish(self, completion: &AsyncCompletion) {
            assert_eq!(self.drops.get(), 1, "captures precede observation");
            assert_eq!(
                crate::test_callback::async_return_calls(),
                1,
                "delivery precedes observation"
            );
            assert!(matches!(
                completion.delivery,
                OwnedDeliveryOutcome::Delivered
            ));
            match (&completion.completion, self.expected) {
                (OwnedCompletionOutcome::Error(error), Failure::Internal)
                    if error.diagnostic_id().is_some() => {}
                (OwnedCompletionOutcome::Error(error), Failure::Panic) if error.is_panic() => {}
                (OwnedCompletionOutcome::Panic(_), Failure::Panic) => {}
                (OwnedCompletionOutcome::Error(error), Failure::Overloaded)
                    if error.is_overloaded() => {}
                _ => panic!("wrong launch completion cause"),
            }
            self.completions.set(self.completions.get() + 1);
        }
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
        // SAFETY: this initialized opaque token is live during its immediate copy.
        unsafe { ExcelAsyncResponder::from_raw("LAUNCH.OWNER.TEST", &mut raw).unwrap() }
    }

    fn verify_failure(panic: bool, reserved_failure: bool) {
        let _fixture = crate::test_callback::lock();
        crate::test_callback::install();
        crate::test_callback::reset();
        let (_cancellation, token) = TaskCancellationOwner::new(CancellationGuarantee::BestEffort);
        let drops = Cell::new(0);
        let completions = Cell::new(0);
        let expected = if reserved_failure {
            Failure::Overloaded
        } else if panic {
            Failure::Panic
        } else {
            Failure::Internal
        };
        let body = CaptureOwner::new(
            Captures {
                drops: &drops,
                panic,
            },
            PendingDelivery::new(
                responder(),
                token,
                Observation {
                    drops: &drops,
                    completions: &completions,
                    expected,
                },
            ),
        );
        if reserved_failure {
            body.fail(XllError::OVERLOADED);
        } else {
            drop(body);
        }
        assert_eq!(drops.get(), 1);
        assert_eq!(completions.get(), 1);
        assert_eq!(crate::test_callback::async_return_calls(), 1);
    }

    #[test]
    fn miri_abandoned_prepared_body_releases_captures_before_one_fallback() {
        verify_failure(false, false);
    }
    #[test]
    fn miri_panicking_prepared_capture_reports_panic_after_release_once() {
        verify_failure(true, false);
    }
    #[test]
    fn miri_reservation_failure_preserves_its_cause_when_capture_drop_panics() {
        verify_failure(true, true);
    }

    #[test]
    fn miri_forgotten_prepared_body_never_claims_delivery_or_capture_destruction() {
        let _fixture = crate::test_callback::lock();
        crate::test_callback::install();
        crate::test_callback::reset();
        let (_cancellation, token) = TaskCancellationOwner::new(CancellationGuarantee::BestEffort);
        let drops = Cell::new(0);
        #[allow(
            clippy::mem_forget,
            reason = "verify safe forget cannot certify delivery or capture destruction"
        )]
        std::mem::forget(CaptureOwner::new(
            Captures {
                drops: &drops,
                panic: false,
            },
            PendingDelivery::new(responder(), token, NoObservation),
        ));
        assert_eq!(drops.get(), 0);
        assert_eq!(crate::test_callback::async_return_calls(), 0);
    }
}
