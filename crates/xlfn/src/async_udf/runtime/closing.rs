//! The sole retiring executor owns both its allocation and its restore target.
use super::*;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

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
    owner: RollbackGuard<RetiringExecutor<'a, E>>,
}
struct RetiringExecutor<'a, E: AsyncExecutor> {
    runtime: &'a AsyncRuntime<E>,
    executor: PublishedOwner<E>,
}
impl<E: AsyncExecutor> Rollback for RetiringExecutor<'_, E> {
    fn rollback(self) {
        // No other transition can change the vacant Closing slot: start rejects
        // it, and other closers wait. Only this private, issued owner can return
        // an executor. Forgetting it retains the vacant state and allocation.
        *self.runtime.state.lock() = ExecutorSlot::Closing(Some(self.executor));
        self.runtime.state_changed.notify_all();
    }
}

impl<'a, E: AsyncExecutor> ExecutorClose<'a, E, PublicationsSealed> {
    pub(super) fn drain_readers(self) -> XllResult<ExecutorClose<'a, E, AwaitingTasks<'a>>> {
        let owner = self.owner;
        let runtime = owner.get().runtime;
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
            self.owner.get().executor.shutdown()
        }))
        .unwrap_or(Err(XllError::Panic))?;
        let Self { state, owner } = self;
        state.registry.finish();
        let RetiringExecutor { runtime, executor } = owner.complete();
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
        self.take_for_close_with_wait(|| {})
    }

    fn take_for_close_with_wait(
        &self,
        mut before_wait: impl FnMut(),
    ) -> Option<ExecutorClose<'_, E, PublicationsSealed>> {
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
                    // Startup reopens the task registry before publishing
                    // Running under this same lock. Seal only after acquiring
                    // that publication; an earlier seal can be overwritten
                    // while this closer waits for Starting to finish.
                    self.registry().close_admission();
                    owner = RollbackGuard::new(RetiringExecutor {
                        runtime: self,
                        executor,
                    });
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
                    before_wait();
                    self.state_changed.wait(&mut state);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Executor;

    impl AsyncExecutor for Executor {
        type Reservation = ();

        fn start(&self) -> XllResult<()> {
            Ok(())
        }

        fn reserve(&self) -> XllResult<()> {
            Ok(())
        }

        fn submit<F: Future<Output = ()> + Send + 'static>(
            &self,
            (): (),
            task: crate::async_udf::AsyncTask<F>,
        ) {
            drop(task);
        }

        fn shutdown(&self) -> XllResult<()> {
            Ok(())
        }
    }

    #[test]
    fn close_waiting_for_startup_seals_the_published_registry() {
        let runtime = AsyncRuntime::new();
        let startup = super::super::startup::StartupClaim::acquire(&runtime, Executor).unwrap();
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let runtime = &runtime;
            let closer = scope.spawn(move || {
                let closing = runtime
                    .take_for_close_with_wait(|| waiting_tx.send(()).unwrap())
                    .unwrap();
                // request_close() in drain_readers would seal it again and
                // mask this race; inspect the owner handoff before that call.
                assert!(matches!(
                    runtime.registry().preflight_available(),
                    Err(XllError::Closing)
                ));
                closing.drain_readers().unwrap().wait().shutdown().unwrap();
            });
            waiting_rx.recv().unwrap();
            startup.publish(crate::AsyncTaskLimit::DEFAULT).unwrap();
            closer.join().unwrap();
        });
        assert!(runtime.is_stopped());
    }
}
