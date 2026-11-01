//! The sole retiring executor owns both its allocation and its restore target.
use super::*;

pub(super) struct PublicationsSealed;
pub(super) struct AwaitingTasks<'a> {
    registry: RegistryClose<'a, TasksCancelled>,
}
pub(super) struct ExecutorTasksDrained<'a> {
    registry: RegistryClose<'a, TasksDrained>,
}

#[must_use = "finish shutdown or retain the exact retired executor"]
pub(super) struct ExecutorClose<'a, E: AsyncExecutor, S> {
    // Return the registry claim before making the executor available to retry.
    state: S,
    owner: ExecutorCloseOwner<'a, E>,
}
struct RetiringExecutor<'a, E: AsyncExecutor> {
    runtime: &'a AsyncRuntime<E>,
    executor: PublishedOwner<E>,
}
impl<E: AsyncExecutor> RetiringExecutor<'_, E> {
    fn restore(self) {
        // No other transition can change the vacant Closing slot: start rejects
        // it, and other closers wait. Only this private, issued owner can return
        // an executor. Forgetting it retains the vacant state and allocation.
        *self.runtime.state.lock() = ExecutorSlot::Closing(Some(self.executor));
        self.runtime.state_changed.notify_all();
    }
}
struct ExecutorCloseOwner<'a, E: AsyncExecutor> {
    pending: Option<RetiringExecutor<'a, E>>,
}
impl<'a, E: AsyncExecutor> ExecutorCloseOwner<'a, E> {
    fn retained(&self) -> &RetiringExecutor<'a, E> {
        self.pending
            .as_ref()
            .expect("retiring owner retains executor")
    }
}
impl<E: AsyncExecutor> Drop for ExecutorCloseOwner<'_, E> {
    fn drop(&mut self) {
        if let Some(retired) = self.pending.take() {
            retired.restore();
        }
    }
}

impl<'a, E: AsyncExecutor> ExecutorClose<'a, E, PublicationsSealed> {
    pub(super) fn drain_readers(self) -> XllResult<ExecutorClose<'a, E, AwaitingTasks<'a>>> {
        let owner = self.owner;
        let runtime = owner.retained().runtime;
        drop(runtime.spawn_admission.control().wait_idle());
        let registry = runtime.registry().request_close()?.cancel();
        Ok(ExecutorClose {
            state: AwaitingTasks { registry },
            owner,
        })
    }
}
impl<'a, E: AsyncExecutor> ExecutorClose<'a, E, AwaitingTasks<'a>> {
    pub(super) fn wait(self) -> ExecutorClose<'a, E, ExecutorTasksDrained<'a>> {
        let Self { state, owner } = self;
        let registry = state.registry.wait();
        ExecutorClose {
            state: ExecutorTasksDrained { registry },
            owner,
        }
    }
    #[cfg(all(test, feature = "async-builtin"))]
    pub(super) fn wait_timeout(
        self,
        timeout: std::time::Duration,
    ) -> XllResult<ExecutorClose<'a, E, ExecutorTasksDrained<'a>>> {
        let Self { state, owner } = self;
        let registry = state.registry.wait_timeout(timeout)?;
        Ok(ExecutorClose {
            state: ExecutorTasksDrained { registry },
            owner,
        })
    }
}
impl<E: AsyncExecutor> ExecutorClose<'_, E, ExecutorTasksDrained<'_>> {
    pub(super) fn shutdown(self) -> XllResult<crate::shutdown::StopOutcome<AsyncStopped>> {
        crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            self.owner.retained().executor.shutdown()
        }))
        .unwrap_or(Err(XllError::Panic))?;
        let Self { state, mut owner } = self;
        state.registry.finish();
        let RetiringExecutor { runtime, executor } = owner
            .pending
            .take()
            .expect("retiring owner retains executor");
        let disposal =
            crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| drop(executor)));
        *runtime.state.lock() = ExecutorSlot::Stopped;
        runtime.state_changed.notify_all();
        let issues = if disposal.is_err() {
            vec![crate::shutdown::CleanupIssue {
                component: "async executor",
                kind: crate::shutdown::CleanupIssueKind::DisposalPanicked,
                error: XllError::Panic,
            }]
        } else {
            Vec::new()
        };
        Ok(crate::shutdown::StopOutcome {
            certificate: AsyncStopped::new(),
            issues,
        })
    }
}

impl<E: AsyncExecutor> AsyncRuntime<E> {
    pub(super) fn take_for_close(&self) -> Option<ExecutorClose<'_, E, PublicationsSealed>> {
        self.registry().close_admission();
        // Recovery is declared first so unwind unlocks state before restoration.
        let owner;
        let mut state = self.state.lock();
        loop {
            match std::mem::replace(&mut *state, ExecutorSlot::Closing(None)) {
                ExecutorSlot::Stopped => {
                    *state = ExecutorSlot::Stopped;
                    return None;
                }
                ExecutorSlot::Running(executor) | ExecutorSlot::Closing(Some(executor)) => {
                    owner = ExecutorCloseOwner {
                        pending: Some(RetiringExecutor {
                            runtime: self,
                            executor,
                        }),
                    };
                    drop(self.spawn_admission.control().seal());
                    self.publication
                        .store(std::ptr::null_mut(), Ordering::Release);
                    self.state_changed.notify_all();
                    return Some(ExecutorClose {
                        state: PublicationsSealed,
                        owner,
                    });
                }
                waiting @ (ExecutorSlot::Starting | ExecutorSlot::Closing(None)) => {
                    *state = waiting;
                    self.state_changed.wait(&mut state);
                }
            }
        }
    }
}
