use super::*;

pub(crate) struct Executor {
    pub(crate) handle: Arc<ExecutorHandle>,
    pub(crate) receiver: Receiver<Runnable>,
    pub(crate) workers: Vec<JoinHandle<()>>,
}

#[derive(Clone)]
pub(crate) struct ExecutorHandle {
    pub(crate) inner: Arc<ExecutorInner>,
    pub(crate) sender: Sender<Runnable>,
}

/// Inner state of `Executor`.
///
/// Invariants:
/// I1. `current`'s `GenerationState` always exists in `control.generations` until `ControlPhase::Closing`.
/// I2. When `control.phase == ControlPhase::Running`, `current.admission` is the admission authority for the current generation.
/// I3. When `control.phase == ControlPhase::Advancing { from, to }`, `current.id == from` and `current.admission` is closed.
/// I4. After `control.phase == ControlPhase::Closing`, no new `GenerationState` is ever published to `current`.
/// I5. A `GenerationState` may be removed from `control.generations` only when `generation != next` and `task_count == 0`.
pub(crate) struct ExecutorInner {
    pub(crate) next_id: AtomicU64,
    pub(crate) active: AtomicUsize,
    pub(crate) live_workers: AtomicUsize,
    pub(crate) fatal_worker_failure: AtomicBool,
    /// Monotonic fast-path mirror of `ExecutorControl::phase == ControlPhase::Closing`.
    ///
    /// Lifecycle transitions are authoritative under `control`;
    /// spawn reads only this atomic.
    pub(crate) closing: AtomicBool,
    pub(crate) current: ArcSwap<GenerationState>,
    /// Cold lifecycle state. Never acquired by spawn/completion.
    pub(crate) control: Mutex<ExecutorControl>,
    pub(crate) wait_lock: Mutex<()>,
    pub(crate) idle: Condvar,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
    #[cfg(test)]
    pub(crate) before_task_schedule_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(crate) after_generation_snapshot_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(crate) after_generation_admission_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Executor {
    pub(crate) fn start(worker_count: usize, generation: u64) -> XllResult<Self> {
        let worker_count = worker_count.clamp(1, 32);
        let (sender, receiver) = async_channel::unbounded::<Runnable>();
        let initial_generation = Arc::new(GenerationState::new(generation));
        let inner = Arc::new(ExecutorInner {
            next_id: AtomicU64::new(1),
            active: AtomicUsize::new(0),
            live_workers: AtomicUsize::new(0),
            fatal_worker_failure: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            current: ArcSwap::from(Arc::clone(&initial_generation)),
            control: Mutex::new(ExecutorControl {
                phase: ControlPhase::Running,
                generations: [(generation, initial_generation)].into_iter().collect(),
            }),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: Mutex::new(None),
            #[cfg(test)]
            before_task_schedule_hook: Mutex::new(None),
            #[cfg(test)]
            after_generation_snapshot_hook: Mutex::new(None),
            #[cfg(test)]
            after_generation_admission_hook: Mutex::new(None),
        });
        let mut workers = scopeguard::guard(
            Vec::<JoinHandle<()>>::with_capacity(worker_count),
            |mut workers| {
                sender.close();
                loop {
                    let Some(worker) = workers.pop() else {
                        break;
                    };
                    drop(worker.join());
                }
            },
        );
        for index in 0..worker_count {
            let receiver = receiver.clone();
            let worker_inner = Arc::clone(&inner);
            inner.live_workers.fetch_add(1, Ordering::Release);
            let worker = thread::Builder::new()
                .name(format!("xlfn-async-{index}"))
                .spawn(move || {
                    let _exit = WorkerExitGuard {
                        inner: worker_inner,
                    };
                    run_executor(receiver);
                });
            let worker = match worker {
                Ok(worker) => worker,
                Err(_) => {
                    inner.live_workers.fetch_sub(1, Ordering::AcqRel);
                    return Err(XllError::Internal {
                        diagnostic_id: 0x4153_594e_4353_504e,
                    });
                }
            };
            workers.push(worker);
        }
        let workers = scopeguard::ScopeGuard::into_inner(workers);
        let handle = Arc::new(ExecutorHandle {
            inner: Arc::clone(&inner),
            sender: sender.clone(),
        });
        Ok(Self {
            handle,
            receiver,
            workers,
        })
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        *self.handle.inner.ghost.lock() = Some(ghost);
    }
}

impl ExecutorHandle {
    pub(crate) fn spawn<F>(
        &self,
        generation: u64,
        future: F,
        cancellation: CancellationSource,
    ) -> Result<(), SpawnRejection<F>>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(SpawnRejection {
                error: XllError::Closing,
                future,
                cancellation,
                cancel: true,
            });
        }

        let current = self.inner.current.load_full();

        #[cfg(test)]
        {
            let hook = self.inner.after_generation_snapshot_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }

        if current.id != generation {
            return Err(SpawnRejection {
                error: cancelled_calculation_error(),
                future,
                cancellation,
                cancel: true,
            });
        }

        let Some(admission) = current.admission.try_enter() else {
            let error = if self.inner.closing.load(Ordering::Acquire) {
                XllError::Closing
            } else {
                cancelled_calculation_error()
            };

            return Err(SpawnRejection {
                error,
                future,
                cancellation,
                cancel: true,
            });
        };

        #[cfg(test)]
        {
            let hook = self.inner.after_generation_admission_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }

        let Some(reservation) = ActiveReservation::try_acquire(Arc::clone(&self.inner)) else {
            drop(admission);
            return Err(SpawnRejection {
                error: XllError::Overloaded,
                future,
                cancellation,
                cancel: false,
            });
        };

        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (abort, registration) = AbortHandle::new_pair();

        let index = task_shard(id);
        {
            let mut tasks = current.shards[index].tasks.lock();
            let previous = tasks.insert(
                id,
                TaskControl {
                    abort,
                    cancellation,
                },
            );
            debug_assert!(previous.is_none(), "task ID must be unique per generation");
            current.task_count.fetch_add(1, Ordering::AcqRel);
        }

        #[allow(
            unused_mut,
            reason = "completion.ghost is mutated only when feature-gated ghost recording is active"
        )]
        let mut completion = reservation.commit(Arc::clone(&current), id);

        drop(admission);

        #[cfg(any(test, feature = "shutdown-refinement"))]
        if let Some(ghost) = self.inner.ghost.lock().as_ref().cloned() {
            ghost.record_event(crate::shutdown_refinement::GhostEvent::StartAsyncTask);
            completion.ghost = Some(ghost);
        }

        let wrapped = async move {
            let _completion = completion;
            #[cfg(any(test, feature = "shutdown-refinement"))]
            let result = Abortable::new(future, registration).await;
            #[cfg(any(test, feature = "shutdown-refinement"))]
            {
                *_completion.completion.lock() = if result.is_ok() {
                    crate::shutdown_refinement::Completion::Completed
                } else {
                    crate::shutdown_refinement::Completion::Canceled
                };
            }
            #[cfg(not(any(test, feature = "shutdown-refinement")))]
            let _ = Abortable::new(future, registration).await;
        };
        #[cfg(test)]
        {
            let hook = self.inner.before_task_schedule_hook.lock().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        let sender = self.sender.clone();
        let schedule = move |runnable| {
            let _ = sender.try_send(runnable);
        };
        let (runnable, task) = async_task::spawn(wrapped, schedule);
        task.detach();
        runnable.schedule();
        Ok(())
    }

    pub(crate) fn cancel_generation(&self, generation: u64) -> Vec<TaskControl> {
        let generation_arc = {
            let control = self.inner.control.lock();
            let Some(state) = control.generations.get(&generation) else {
                return Vec::new();
            };
            debug_assert_eq!(state.id, generation);
            state.admission.close();
            Arc::clone(state)
        };
        generation_arc.admission.wait_for_idle();
        generation_arc.drain_tasks()
    }

    pub(crate) fn advance_generation(&self, next: u64) -> bool {
        let old = {
            let mut control = self.inner.control.lock();
            match control.phase {
                ControlPhase::Running => {}
                ControlPhase::Closing => return false,
                ControlPhase::Advancing { .. } => {
                    debug_assert!(false, "concurrent executor generation transition");
                    return false;
                }
            }

            let old = self.inner.current.load_full();
            old.admission.close();
            control.phase = ControlPhase::Advancing {
                from: old.id,
                to: next,
            };
            old
        };

        old.admission.wait_for_idle();

        let mut control = self.inner.control.lock();
        match control.phase {
            ControlPhase::Closing => return false,
            ControlPhase::Advancing { from, to } if from == old.id && to == next => {}
            ControlPhase::Running | ControlPhase::Advancing { .. } => {
                debug_assert!(false, "executor generation transition state diverged");
                return false;
            }
        }

        let next_generation = control
            .generations
            .entry(next)
            .or_insert_with(|| Arc::new(GenerationState::new(next)))
            .clone();

        self.inner.current.store(Arc::clone(&next_generation));

        control.generations.retain(|generation, state| {
            *generation == next || state.task_count.load(Ordering::Acquire) != 0
        });

        control.phase = ControlPhase::Running;
        true
    }

    pub(crate) fn request_close(&self) -> Vec<TaskControl> {
        let generations = {
            let mut control = self.inner.control.lock();

            if matches!(control.phase, ControlPhase::Closing) {
                return Vec::new();
            }

            self.inner.closing.store(true, Ordering::Release);
            control.phase = ControlPhase::Closing;

            let generations = control.generations.values().cloned().collect::<Vec<_>>();
            for generation in &generations {
                generation.admission.close();
            }
            generations
        };

        for generation in &generations {
            generation.admission.wait_for_idle();
        }

        let mut tasks = Vec::new();
        for generation in generations {
            tasks.extend(generation.drain_tasks());
        }
        tasks
    }
}

impl Executor {
    pub(crate) fn wait_for_idle(&self) -> bool {
        let mut guard = self.handle.inner.wait_lock.lock();
        while self.handle.inner.active.load(Ordering::Acquire) != 0 {
            if self
                .handle
                .inner
                .fatal_worker_failure
                .load(Ordering::Acquire)
                && self.handle.inner.live_workers.load(Ordering::Acquire) == 0
            {
                return false;
            }
            self.handle.inner.idle.wait(&mut guard);
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn wait_for_idle_timeout(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut guard = self.handle.inner.wait_lock.lock();
        while self.handle.inner.active.load(Ordering::Acquire) != 0 {
            if self
                .handle
                .inner
                .fatal_worker_failure
                .load(Ordering::Acquire)
                && self.handle.inner.live_workers.load(Ordering::Acquire) == 0
            {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            self.handle.inner.idle.wait_for(&mut guard, deadline - now);
        }
        true
    }

    pub(crate) fn drain_after_worker_failure(&self) -> bool {
        self.handle.sender.close();
        while let Ok(runnable) = self.receiver.try_recv() {
            drop(runnable);
        }
        self.handle.inner.active.load(Ordering::Acquire) == 0
    }

    pub(crate) fn finish_close(mut self) -> Vec<crate::shutdown::CleanupIssue> {
        self.handle.sender.close();
        let mut issues = Vec::new();
        for worker in self.workers.drain(..) {
            if worker.join().is_err() {
                issues.push(crate::shutdown::CleanupIssue {
                    component: "async worker",
                    kind: crate::CleanupIssueKind::WorkerPanickedAfterJoin,
                    error: XllError::Panic,
                });
            }
        }
        issues
    }
}
