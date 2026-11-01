//! A framework launch retains its issuer, calculation and cancellation claim
//! across user preparation. Prepared values cannot select another source or
//! execution generation when they are finally submitted.
use super::*;
use crate::async_udf::registry::CalculationSnapshot;
use crate::async_udf::runtime::RuntimeReservation;
use crate::cancellation::TaskCancellationOwner;

pub(super) struct LaunchOwner<'guard, 'call, A: crate::Addin> {
    guard: &'guard crate::runtime::AsyncCallGuard<'call, A>,
    calculation: Option<CalculationSnapshot<'static>>,
    cancellation: Option<TaskCancellationOwner>,
    token: CancellationToken,
}

pub(super) struct PreparedLaunch<'owner, 'guard, 'call, A: crate::Addin, B> {
    owner: &'owner mut LaunchOwner<'guard, 'call, A>,
    body: B,
}

pub(super) struct ReservedLaunch<'owner, 'guard, 'call, A: crate::Addin, B> {
    owner: &'owner mut LaunchOwner<'guard, 'call, A>,
    reservation: RuntimeReservation<'static, A::AsyncExecutor>,
    body: B,
}

pub(super) struct Plain<F>(F);
#[cfg(feature = "handles")]
pub(super) struct Scoped<B> {
    build: B,
    generation: crate::generation::RuntimeGeneration,
}

impl<'guard, 'call, A: crate::Addin> LaunchOwner<'guard, 'call, A> {
    pub(super) fn new(
        guard: &'guard crate::runtime::AsyncCallGuard<'call, A>,
        calculation: CalculationSnapshot<'static>,
    ) -> Self {
        if !calculation.belongs_to(guard.async_runtime().registry()) {
            xlfn_kernel::invariant::fail_stop();
        }
        let (cancellation, token) = TaskCancellationOwner::new(CancellationGuarantee::BestEffort);
        Self {
            guard,
            calculation: Some(calculation),
            cancellation: Some(cancellation),
            token,
        }
    }

    pub(super) fn token(&self) -> CancellationToken {
        self.token
    }

    pub(super) fn prepare<F>(
        &mut self,
        start: impl FnOnce(
            &crate::runtime::CallGuard<'_, A>,
            crate::generation::ExecutionLease<A>,
            CancellationToken,
        ) -> XllResult<F>,
    ) -> XllResult<PreparedLaunch<'_, 'guard, 'call, A, Plain<F>>> {
        self.guard.async_runtime().preflight_snapshot(
            self.calculation
                .as_ref()
                .expect("launch retains calculation"),
        )?;
        let lease = self.guard.try_execution_lease()?;
        let future = start(self.guard, lease, self.token)?;
        Ok(PreparedLaunch {
            owner: self,
            body: Plain(future),
        })
    }

    #[cfg(feature = "handles")]
    pub(super) fn prepare_scoped<B>(
        &mut self,
        start: impl FnOnce(
            &crate::runtime::CallGuard<'_, A>,
            crate::generation::ExecutionLease<A>,
            CancellationToken,
        ) -> XllResult<B>,
    ) -> XllResult<PreparedLaunch<'_, 'guard, 'call, A, Scoped<B>>> {
        self.guard.async_runtime().preflight_snapshot(
            self.calculation
                .as_ref()
                .expect("launch retains calculation"),
        )?;
        let lease = self.guard.try_execution_lease()?;
        let generation = lease.generation();
        let build = start(self.guard, lease, self.token)?;
        Ok(PreparedLaunch {
            owner: self,
            body: Scoped { build, generation },
        })
    }
}

impl<'owner, 'guard, 'call, A: crate::Addin, B> PreparedLaunch<'owner, 'guard, 'call, A, B> {
    pub(super) fn reserve(
        self,
    ) -> Result<ReservedLaunch<'owner, 'guard, 'call, A, B>, (B, XllError)> {
        let Self { owner, body } = self;
        let calculation = owner.calculation.take().expect("one launch reservation");
        match owner.guard.async_runtime().reserve_snapshot(calculation) {
            Ok(reservation) => Ok(ReservedLaunch {
                owner,
                reservation,
                body,
            }),
            Err(error) => Err((body, error)),
        }
    }
}

impl<A: crate::Addin, F, T> ReservedLaunch<'_, '_, '_, A, Plain<F>>
where
    F: Future<Output = XllResult<T>> + Send + 'static,
    T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
{
    pub(super) fn commit<O: CompletionObserver + Send + 'static>(
        self,
        responder: ExcelAsyncResponder,
        observation: O,
    ) {
        let cancellation = self
            .owner
            .cancellation
            .take()
            .expect("one framework task claim");
        let future = AsyncDelivery::new(responder, self.owner.token, observation).task(self.body.0);
        self.reservation.commit_owned(future, cancellation);
    }
}

#[cfg(feature = "handles")]
impl<A: crate::Addin, B> ReservedLaunch<'_, '_, '_, A, Scoped<B>> {
    pub(super) fn commit<T, O: CompletionObserver + Send + 'static>(
        self,
        responder: ExcelAsyncResponder,
        observation: O,
    ) where
        B: HandleScopedBuilder<T> + Send + 'static,
        T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
    {
        let cancellation = self
            .owner
            .cancellation
            .take()
            .expect("one framework task claim");
        let build = HandleTask::new(self.body.build, responder, self.owner.token, observation);
        let generation = self.body.generation;
        let future = async move {
            let brand = crate::handle::GenerationLeaseBrand;
            let scope = super::super::task_scope::AsyncTaskScope::new(generation, &brand);
            let (future, delivery) = build.build_task(scope);
            HandleTask::<B, O, T>::deliver(delivery, future).await;
        };
        self.reservation.commit_owned(future, cancellation);
    }
}
