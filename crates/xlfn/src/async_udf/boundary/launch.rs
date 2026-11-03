//! One framework launch owns its calculation, cancellation and copied response
//! handle through preparation and reservation. No parent survives a transition.
use super::*;
use crate::async_udf::runtime::RuntimeCalculationSnapshot;
use crate::async_udf::runtime::RuntimeReservation;
use crate::cancellation::TaskCancellationOwner;

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
    body: LaunchBody<B, O>,
}

pub(super) struct ReservedLaunch<'guard, 'call, A: crate::Addin, B, O: CompletionObserver> {
    _guard: &'guard crate::runtime::AsyncCallGuard<'call, A>,
    reservation: RuntimeReservation<'static, A::AsyncExecutor>,
    cancellation: TaskCancellationOwner,
    body: LaunchBody<B, O>,
}

/// Taking these fields disarms cleanup; otherwise user captures are destroyed
/// before fallback delivery and observation, even when their destructor panics.
struct LaunchBody<B, O: CompletionObserver> {
    body: Option<B>,
    delivery: Option<AsyncDelivery<O>>,
}

impl<B, O: CompletionObserver> LaunchBody<B, O> {
    fn new(body: B, delivery: AsyncDelivery<O>) -> Self {
        Self {
            body: Some(body),
            delivery: Some(delivery),
        }
    }

    fn into_parts(mut self) -> (B, AsyncDelivery<O>) {
        (
            self.body.take().expect("one prepared body"),
            self.delivery.take().expect("one launch delivery"),
        )
    }

    fn fail(mut self, error: XllError) {
        // Reservation failure remains the completion cause even if destroying
        // the rejected user's captures also panics, matching launch policy.
        let _ = catch_no_unwind(AssertUnwindSafe(|| drop(self.body.take())));
        self.delivery
            .take()
            .expect("one launch delivery")
            .fail(error);
    }
}

impl<B, O: CompletionObserver> Drop for LaunchBody<B, O> {
    fn drop(&mut self) {
        let panicked = catch_no_unwind(AssertUnwindSafe(|| drop(self.body.take()))).is_err();
        if let Some(delivery) = &mut self.delivery {
            delivery.fallback(panicked);
        }
    }
}

pub(super) struct Plain<F>(F);
#[cfg(feature = "handles")]
pub(super) struct Scoped<B> {
    build: B,
    generation: crate::generation::RuntimeGeneration,
}

impl<'guard, 'call, A: crate::Addin> LaunchOwner<'guard, 'call, A> {
    pub(super) fn token(&self) -> CancellationToken {
        self.token
    }

    pub(super) fn prepare<F, O: CompletionObserver>(
        self,
        start: impl FnOnce(
            &crate::runtime::CallGuard<'_, A>,
            crate::generation::ExecutionLease<A>,
            CancellationToken,
        ) -> XllResult<F>,
        responder: ExcelAsyncResponder,
        observation: O,
    ) -> Option<PreparedLaunch<'guard, 'call, A, Plain<F>, O>> {
        // Bind the copied responder before any user preparation can run.
        let delivery = AsyncDelivery::new(responder, self.token, observation);
        let body = catch_no_unwind(AssertUnwindSafe(|| {
            self.calculation.preflight()?;
            let lease = self.guard.try_execution_lease()?;
            start(self.guard, lease, self.token).map(Plain)
        }))
        .unwrap_or(Err(XllError::Panic));
        match body {
            Ok(body) => Some(PreparedLaunch {
                owner: self,
                body: LaunchBody::new(body, delivery),
            }),
            Err(error) => {
                delivery.fail(error);
                None
            }
        }
    }

    #[cfg(feature = "handles")]
    pub(super) fn prepare_scoped<B, O: CompletionObserver>(
        self,
        start: impl FnOnce(
            &crate::runtime::CallGuard<'_, A>,
            crate::generation::ExecutionLease<A>,
            CancellationToken,
        ) -> XllResult<B>,
        responder: ExcelAsyncResponder,
        observation: O,
    ) -> Option<PreparedLaunch<'guard, 'call, A, Scoped<B>, O>> {
        let delivery = AsyncDelivery::new(responder, self.token, observation);
        let body = catch_no_unwind(AssertUnwindSafe(|| {
            self.calculation.preflight()?;
            let lease = self.guard.try_execution_lease()?;
            let generation = lease.generation();
            start(self.guard, lease, self.token).map(|build| Scoped { build, generation })
        }))
        .unwrap_or(Err(XllError::Panic));
        match body {
            Ok(body) => Some(PreparedLaunch {
                owner: self,
                body: LaunchBody::new(body, delivery),
            }),
            Err(error) => {
                delivery.fail(error);
                None
            }
        }
    }
}

impl<'guard, 'call, A: crate::Addin, B, O: CompletionObserver>
    PreparedLaunch<'guard, 'call, A, B, O>
{
    pub(super) fn reserve(self) -> Option<ReservedLaunch<'guard, 'call, A, B, O>> {
        let Self { owner, body } = self;
        match owner.calculation.reserve() {
            Ok(reservation) => Some(ReservedLaunch {
                _guard: owner.guard,
                reservation,
                cancellation: owner.cancellation,
                body,
            }),
            Err(error) => {
                body.fail(error);
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
        let (body, delivery) = self.body.into_parts();
        self.reservation
            .commit_owned(delivery.task(body.0), self.cancellation);
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
        let (body, delivery) = self.body.into_parts();
        let build = HandleTask::with_delivery(body.build, delivery);
        let generation = body.generation;
        let future = async move {
            let brand = crate::handle::GenerationLeaseBrand;
            let scope = super::super::task_scope::AsyncTaskScope::new(generation, &brand);
            let (future, delivery) = build.build_task(scope);
            HandleTask::<B, O, T>::deliver(delivery, future).await;
        };
        self.reservation.commit_owned(future, self.cancellation);
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
        fn finish(&mut self, completion: &AsyncCompletion) {
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
                (OwnedCompletionOutcome::Error(XllError::Internal { .. }), Failure::Internal)
                | (OwnedCompletionOutcome::Error(XllError::Panic), Failure::Panic)
                | (OwnedCompletionOutcome::Error(XllError::Overloaded), Failure::Overloaded) => {}
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
        let body = LaunchBody::new(
            Captures {
                drops: &drops,
                panic,
            },
            AsyncDelivery::new(
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
            body.fail(XllError::Overloaded);
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
        std::mem::forget(LaunchBody::new(
            Captures {
                drops: &drops,
                panic: false,
            },
            AsyncDelivery::new(responder(), token, NoObservation),
        ));
        assert_eq!(drops.get(), 0);
        assert_eq!(crate::test_callback::async_return_calls(), 0);
    }
}
