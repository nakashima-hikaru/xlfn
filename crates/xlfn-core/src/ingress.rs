#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
#[cfg(test)]
use std::thread::ThreadId;

pub const PHASE_OPENING: u8 = 0;
pub const PHASE_OPEN: u8 = 1;
pub const PHASE_CLOSING: u8 = 2;
pub const PHASE_CLOSED: u8 = 3;

const PHASE_SHIFT: u32 = 62;
const EXPORT_SHIFT: u32 = 32;
const EXPORT_BITS: u32 = 30;
const UDF_BITS: u32 = 32;

const UDF_MASK: u64 = (1_u64 << UDF_BITS) - 1;
const EXPORT_MASK: u64 = ((1_u64 << EXPORT_BITS) - 1) << EXPORT_SHIFT;
const ACTIVE_MASK: u64 = (1_u64 << PHASE_SHIFT) - 1;

const EXPORT_ONE: u64 = 1_u64 << EXPORT_SHIFT;
const UDF_ONE: u64 = 1_u64;

#[cfg(test)]
struct TestEpochGate {
    owner: Mutex<Option<(ThreadId, usize)>>,
    idle: Condvar,
}

#[cfg(test)]
impl TestEpochGate {
    const fn new() -> Self {
        Self {
            owner: Mutex::new(None),
            idle: Condvar::new(),
        }
    }

    fn acquire(&self) {
        let current = std::thread::current().id();
        let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            match owner.as_mut() {
                None => {
                    *owner = Some((current, 1));
                    return;
                }
                Some((existing, depth)) if *existing == current => {
                    *depth += 1;
                    return;
                }
                Some(_) => {
                    owner = self
                        .idle
                        .wait(owner)
                        .unwrap_or_else(|error| error.into_inner());
                }
            }
        }
    }

    fn release(&self) {
        let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
        match owner.as_mut() {
            Some((_, depth)) if *depth > 1 => *depth -= 1,
            Some(_) => {
                *owner = None;
                self.idle.notify_one();
            }
            None => panic!("module test gate released without an owner"),
        }
    }
}

#[cfg(test)]
static TEST_EPOCH_GATE: TestEpochGate = TestEpochGate::new();

#[cfg(test)]
pub(crate) struct TestModuleLease;

#[cfg(test)]
pub(crate) fn acquire_test_module_lease() -> TestModuleLease {
    TEST_EPOCH_GATE.acquire();
    TestModuleLease
}

#[cfg(test)]
impl Drop for TestModuleLease {
    fn drop(&mut self) {
        TEST_EPOCH_GATE.release();
    }
}

const fn pack_state(phase: u8, active: u64) -> u64 {
    ((phase as u64) << PHASE_SHIFT) | active
}

const fn phase_of(state: u64) -> u8 {
    (state >> PHASE_SHIFT) as u8
}

const fn active_exports_of(state: u64) -> u64 {
    (state & EXPORT_MASK) >> EXPORT_SHIFT
}

const fn active_udfs_of(state: u64) -> u64 {
    state & UDF_MASK
}

const fn active_of(state: u64) -> u64 {
    state & ACTIVE_MASK
}

/// Proof token certifying that all module export entries have been drained.
#[derive(Debug)]
pub struct ExportsDrained {
    #[allow(
        dead_code,
        reason = "Linear proof token tracking epoch for ingress drain"
    )]
    pub(crate) epoch: u64,
}

/// Global ingress manager tracking all external DLL export calls entering the XLL.
///
/// Calls that arrive while OPENING or CLOSING are counted until their rejection
/// path has returned. Calls that arrive after the ingress is sealed CLOSED
/// cannot become active, which makes the drain certificate linearizable with
/// the CLOSED transition.
#[derive(Debug)]
pub struct ExportIngress {
    // The phase and active-call count share one atomic word. This makes entry
    // independent of the shutdown mutex while preserving a linearization
    // point between an entry and the closing transition.
    state: AtomicU64,
    epoch: AtomicU64,
    // The mutex is used only by the rare shutdown wait and by the final guard
    // drop that wakes that wait. It is not taken by ordinary entry calls.
    wait_lock: Mutex<()>,
    idle: Condvar,
    // Opening entries are rejected by the caller but counted until their
    // guards leave. Publication takes this lock so the zero-active check and
    // the final OPEN transition cannot be overtaken by a new entry.
    opening_lock: Mutex<()>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    // Refinement hooks must be serialized with the ingress CAS. Otherwise a
    // thread accepted by `enter` can be descheduled before its ghost event is
    // recorded and let `begin_close` overtake that event.
    linearization_lock: Mutex<()>,
    #[cfg(test)]
    close_waiters: AtomicUsize,
    #[cfg(test)]
    test_epoch_active: AtomicUsize,
}

#[derive(Debug)]
pub(crate) struct OpeningPublicationLost;

static GLOBAL_INGRESS: ExportIngress = ExportIngress::new();
static DIAGNOSTIC_LINEARIZATION: Mutex<()> = Mutex::new(());

pub fn global_ingress() -> &'static ExportIngress {
    &GLOBAL_INGRESS
}

pub(crate) fn with_diagnostic_linearization<F, R>(operation: F) -> R
where
    F: FnOnce() -> R,
{
    let _guard = DIAGNOSTIC_LINEARIZATION
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    operation()
}

impl Default for ExportIngress {
    fn default() -> Self {
        Self::new()
    }
}

impl ExportIngress {
    pub const fn new() -> Self {
        Self {
            state: AtomicU64::new(pack_state(PHASE_CLOSED, 0)),
            epoch: AtomicU64::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
            opening_lock: Mutex::new(()),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            linearization_lock: Mutex::new(()),
            #[cfg(test)]
            close_waiters: AtomicUsize::new(0),
            #[cfg(test)]
            test_epoch_active: AtomicUsize::new(0),
        }
    }

    /// Starts a new ingress epoch in the non-admitting opening phase.
    pub fn begin_opening(&self) {
        let _opening_guard = self
            .opening_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        #[cfg(test)]
        let owns_test_epoch = std::ptr::eq(self, global_ingress());
        #[cfg(test)]
        if owns_test_epoch {
            TEST_EPOCH_GATE.acquire();
            self.test_epoch_active.store(1, Ordering::Release);
        }
        let state = self.state.load(Ordering::Acquire);
        if phase_of(state) != PHASE_CLOSED || active_of(state) != 0 {
            #[cfg(test)]
            if owns_test_epoch {
                self.test_epoch_active.store(0, Ordering::Release);
                TEST_EPOCH_GATE.release();
            }
            assert_eq!(phase_of(state), PHASE_CLOSED, "ingress opening before seal");
            assert_eq!(
                active_of(state),
                0,
                "ingress opening with live export guards"
            );
        }
        self.epoch
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                epoch.checked_add(1)
            })
            .unwrap_or_else(|_| std::process::abort());
        self.state
            .store(pack_state(PHASE_OPENING, 0), Ordering::Release);
    }

    /// Completes opening as one publication transaction.
    ///
    /// The callback runs while external entry is still rejected. Once it
    /// succeeds, the ingress is atomically published as OPEN. A rejected
    /// Opening entry cannot race the zero-active observation because it must
    /// acquire the same opening lock first.
    pub(crate) fn complete_open<F, E>(
        &self,
        operation: F,
    ) -> Result<Result<(), E>, OpeningPublicationLost>
    where
        F: FnOnce() -> Result<(), E>,
    {
        let _opening_guard = self
            .opening_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        #[cfg(any(test, feature = "shutdown-refinement"))]
        let _linearization_guard = self
            .linearization_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        let mut wait_guard = self
            .wait_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while active_exports_of(self.state.load(Ordering::Acquire)) != 0 {
            wait_guard = self
                .idle
                .wait(wait_guard)
                .unwrap_or_else(|error| error.into_inner());
        }
        drop(wait_guard);

        if phase_of(self.state.load(Ordering::Acquire)) != PHASE_OPENING {
            return Err(OpeningPublicationLost);
        }

        let result = operation();
        if result.is_err() {
            return Ok(result);
        }
        match self.state.compare_exchange(
            pack_state(PHASE_OPENING, 0),
            pack_state(PHASE_OPEN, 0),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(result),
            Err(_) => Err(OpeningPublicationLost),
        }
    }

    /// Attempts to enter an export entry and runs `on_accepted` at the same
    /// refinement linearization point as the accepting state transition.
    pub fn enter_with<F>(&self, on_accepted: F) -> (ExportCallGuard<'_>, bool)
    where
        F: FnOnce(),
    {
        let observed_phase = phase_of(self.state.load(Ordering::Acquire));
        let _opening_guard = (observed_phase == PHASE_OPENING).then(|| {
            self.opening_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        });
        #[cfg(any(test, feature = "shutdown-refinement"))]
        let _linearization_guard = self
            .linearization_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut on_accepted = Some(on_accepted);
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            let phase = phase_of(observed);
            if phase == PHASE_CLOSED {
                return (
                    ExportCallGuard {
                        ingress: self,
                        epoch: self.epoch.load(Ordering::Acquire),
                        decrement: 0,
                    },
                    false,
                );
            }

            let active_exports = active_exports_of(observed);
            if active_exports == ((1_u64 << EXPORT_BITS) - 1) {
                std::process::abort();
            }
            let next = observed + EXPORT_ONE;
            match self.state.compare_exchange_weak(
                observed,
                next,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    let accepted = phase == PHASE_OPEN;
                    if accepted {
                        let hook = on_accepted
                            .take()
                            .expect("ingress acceptance hook called once");
                        hook();
                    }
                    return (
                        ExportCallGuard {
                            ingress: self,
                            epoch: self.epoch.load(Ordering::Acquire),
                            decrement: EXPORT_ONE,
                        },
                        accepted,
                    );
                }
                Err(current) => observed = current,
            }
        }
    }

    /// Attempts to enter a UDF export entry and runs `on_accepted` at the same
    /// refinement linearization point as the accepting state transition.
    /// Returns `(guard, accepted, concurrent_calls)`.
    pub fn enter_udf_with<F>(&self, on_accepted: F) -> (ExportCallGuard<'_>, bool, usize)
    where
        F: FnOnce(),
    {
        let observed_phase = phase_of(self.state.load(Ordering::Acquire));
        let _opening_guard = (observed_phase == PHASE_OPENING).then(|| {
            self.opening_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        });
        #[cfg(any(test, feature = "shutdown-refinement"))]
        let _linearization_guard = self
            .linearization_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut on_accepted = Some(on_accepted);
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            let phase = phase_of(observed);
            if phase == PHASE_CLOSED {
                return (
                    ExportCallGuard {
                        ingress: self,
                        epoch: self.epoch.load(Ordering::Acquire),
                        decrement: 0,
                    },
                    false,
                    0,
                );
            }

            let active_exports = active_exports_of(observed);
            let active_udfs = active_udfs_of(observed);
            if active_exports == ((1_u64 << EXPORT_BITS) - 1) || active_udfs == UDF_MASK {
                std::process::abort();
            }
            let next = observed + EXPORT_ONE + UDF_ONE;
            match self.state.compare_exchange_weak(
                observed,
                next,
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    let accepted = phase == PHASE_OPEN;
                    if accepted {
                        let hook = on_accepted
                            .take()
                            .expect("ingress acceptance hook called once");
                        hook();
                    }
                    let concurrent_calls = active_udfs_of(next) as usize;
                    return (
                        ExportCallGuard {
                            ingress: self,
                            epoch: self.epoch.load(Ordering::Acquire),
                            decrement: EXPORT_ONE + UDF_ONE,
                        },
                        accepted,
                        concurrent_calls,
                    );
                }
                Err(current) => observed = current,
            }
        }
    }

    /// Stops accepting new export calls and runs `on_closed` at the same
    /// refinement linearization point as the closing state transition.
    pub fn begin_close_with<F>(&self, on_closed: F)
    where
        F: FnOnce(),
    {
        self.begin_close_with_inner(|| {}, on_closed);
    }

    #[cfg(test)]
    fn begin_close_with_attempt_hook<F, G>(&self, on_attempt: F, on_closed: G)
    where
        F: FnOnce(),
        G: FnOnce(),
    {
        self.begin_close_with_inner(on_attempt, on_closed);
    }

    fn begin_close_with_inner<F, G>(&self, on_attempt: F, on_closed: G)
    where
        F: FnOnce(),
        G: FnOnce(),
    {
        on_attempt();
        #[cfg(test)]
        self.close_waiters.fetch_add(1, Ordering::AcqRel);
        let observed_phase = phase_of(self.state.load(Ordering::Acquire));
        let _opening_guard = (observed_phase == PHASE_OPENING).then(|| {
            self.opening_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        });
        #[cfg(any(test, feature = "shutdown-refinement"))]
        let _linearization_guard = self
            .linearization_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        #[cfg(test)]
        self.close_waiters.fetch_sub(1, Ordering::AcqRel);
        with_diagnostic_linearization(|| {
            let mut on_closed = Some(on_closed);
            let mut observed = self.state.load(Ordering::Acquire);
            loop {
                if !matches!(phase_of(observed), PHASE_OPEN | PHASE_OPENING) {
                    return;
                }
                let next = pack_state(PHASE_CLOSING, active_of(observed));
                match self.state.compare_exchange_weak(
                    observed,
                    next,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        let hook = on_closed.take().expect("ingress close hook called once");
                        hook();
                        return;
                    }
                    Err(current) => observed = current,
                }
            }
        });
    }

    /// Runs a refinement-sensitive operation in the same serialization domain
    /// as ingress admission and close initiation.
    pub fn with_linearization<F, R>(&self, operation: F) -> R
    where
        F: FnOnce() -> R,
    {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        let _linearization_guard = self
            .linearization_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        operation()
    }

    /// Waits for the current epoch to drain and seals it CLOSED in the same
    /// synchronization region that observes `active == 0`.
    pub fn seal_and_drain(&self) -> ExportsDrained {
        self.seal_and_drain_with_hook(|| {})
    }

    fn seal_and_drain_with_hook<F>(&self, before_close: F) -> ExportsDrained
    where
        F: FnOnce(),
    {
        assert!(
            !matches!(
                phase_of(self.state.load(Ordering::Acquire)),
                PHASE_OPEN | PHASE_OPENING
            ),
            "ingress sealed before begin_close"
        );
        let mut wait_guard = self.wait_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut before_close = Some(before_close);
        loop {
            while active_exports_of(self.state.load(Ordering::Acquire)) != 0 {
                wait_guard = self
                    .idle
                    .wait(wait_guard)
                    .unwrap_or_else(|e| e.into_inner());
            }

            if let Some(before_close) = before_close.take() {
                before_close();
            }

            // An entry arriving after the active==0 observation is still
            // allowed in CLOSING. Require the exact zero-active state so that
            // such an entry makes this CAS fail instead of being discarded.
            match self.state.compare_exchange(
                pack_state(PHASE_CLOSING, 0),
                pack_state(PHASE_CLOSED, 0),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => match phase_of(observed) {
                    PHASE_CLOSED => break,
                    PHASE_CLOSING => continue,
                    PHASE_OPEN | PHASE_OPENING => panic!("ingress sealed before begin_close"),
                    _ => std::process::abort(),
                },
            }
        }
        drop(wait_guard);
        #[cfg(test)]
        if std::ptr::eq(self, global_ingress())
            && self.test_epoch_active.swap(0, Ordering::AcqRel) != 0
        {
            TEST_EPOCH_GATE.release();
        }
        ExportsDrained {
            epoch: self.epoch.load(Ordering::Acquire),
        }
    }

    pub fn phase(&self) -> u8 {
        phase_of(self.state.load(Ordering::Acquire))
    }

    pub(crate) fn allows_diagnostic_mutation(&self) -> bool {
        let state = self.state.load(Ordering::Acquire);
        phase_of(state) == PHASE_OPENING
            || phase_of(state) == PHASE_OPEN
            || (phase_of(state) == PHASE_CLOSED && self.epoch.load(Ordering::Acquire) == 0)
    }

    pub fn active_calls(&self) -> usize {
        active_exports_of(self.state.load(Ordering::Acquire)) as usize
    }

    pub fn active_udfs(&self) -> usize {
        active_udfs_of(self.state.load(Ordering::Acquire)) as usize
    }
}

pub struct ExportCallGuard<'a> {
    ingress: &'a ExportIngress,
    epoch: u64,
    decrement: u64,
}

impl Drop for ExportCallGuard<'_> {
    fn drop(&mut self) {
        if self.decrement == 0 {
            return;
        }
        assert_eq!(
            self.ingress.epoch.load(Ordering::Acquire),
            self.epoch,
            "export guard crossed ingress epochs"
        );
        let previous = self
            .ingress
            .state
            .fetch_sub(self.decrement, Ordering::Release);
        let active_exports = active_exports_of(previous);
        if active_exports == 0 {
            std::process::abort();
        }
        if active_exports == 1 {
            // Acquiring this lock only for the final active call closes the
            // notify/wait race without putting a mutex on the UDF entry path.
            let _wait_guard = self
                .ingress
                .wait_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.ingress.idle.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, mpsc};

    #[test]
    fn seal_is_linearized_with_the_last_active_guard() {
        let ingress = Arc::new(ExportIngress::new());
        ingress.begin_opening();
        ingress.begin_close_with(|| {});
        let (guard, accepted) = ingress.enter_with(|| {});
        assert!(!accepted);

        let sealing = Arc::clone(&ingress);
        let worker = std::thread::spawn(move || sealing.seal_and_drain());
        std::thread::yield_now();
        assert_eq!(ingress.active_calls(), 1);
        drop(guard);
        let certificate = worker.join().unwrap();

        assert_eq!(certificate.epoch, 1);
        assert_eq!(ingress.phase(), PHASE_CLOSED);
        assert_eq!(ingress.active_calls(), 0);
        let (closed_guard, accepted) = ingress.enter_with(|| {});
        assert!(!accepted);
        assert_eq!(ingress.active_calls(), 0);
        drop(closed_guard);
    }

    #[test]
    fn close_cannot_overtake_an_accepted_entry_hook() {
        let ingress = Arc::new(ExportIngress::new());
        ingress.begin_opening();
        ingress.complete_open(|| Ok::<(), ()>(())).unwrap().unwrap();
        let (hook_started_tx, hook_started_rx) = mpsc::sync_channel(1);
        let (release_hook_tx, release_hook_rx) = mpsc::sync_channel(1);
        let entering = Arc::clone(&ingress);
        let entry = std::thread::spawn(move || {
            let (guard, accepted) = entering.enter_with(|| {
                hook_started_tx.send(()).unwrap();
                release_hook_rx.recv().unwrap();
            });
            assert!(accepted);
            drop(guard);
        });

        hook_started_rx.recv().unwrap();
        let closing = Arc::clone(&ingress);
        let (close_attempt_tx, close_attempt_rx) = mpsc::sync_channel(1);
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let close = std::thread::spawn(move || {
            closing.begin_close_with_attempt_hook(
                || close_attempt_tx.send(()).unwrap(),
                || closed_tx.send(()).unwrap(),
            );
        });
        close_attempt_rx.recv().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while ingress.close_waiters.load(Ordering::Acquire) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "close thread did not reach the linearization lock"
            );
            std::thread::yield_now();
        }
        assert!(matches!(
            closed_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        release_hook_tx.send(()).unwrap();
        entry.join().unwrap();
        close.join().unwrap();
        closed_rx.recv().unwrap();
        assert_eq!(ingress.phase(), PHASE_CLOSING);
    }

    #[test]
    fn opening_starts_a_distinct_epoch_only_after_seal() {
        let ingress = ExportIngress::new();
        ingress.begin_opening();
        ingress.begin_close_with(|| {});
        let first = ingress.seal_and_drain();
        ingress.begin_opening();
        ingress.complete_open(|| Ok::<(), ()>(())).unwrap().unwrap();
        let (guard, accepted) = ingress.enter_with(|| {});
        assert!(accepted);
        drop(guard);
        ingress.begin_close_with(|| {});
        let second = ingress.seal_and_drain();
        assert!(second.epoch > first.epoch);
    }

    #[test]
    fn open_publication_waits_for_rejected_opening_entries() {
        let ingress = Arc::new(ExportIngress::new());
        ingress.begin_opening();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let entering = Arc::clone(&ingress);
        let entry = std::thread::spawn(move || {
            let (guard, accepted) = entering.enter_with(|| {});
            assert!(!accepted);
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(guard);
        });
        entered_rx.recv().unwrap();

        let publishing = Arc::clone(&ingress);
        let (published_tx, published_rx) = mpsc::sync_channel(1);
        let publisher = std::thread::spawn(move || {
            publishing
                .complete_open(|| {
                    published_tx.send(()).unwrap();
                    Ok::<(), ()>(())
                })
                .unwrap()
                .unwrap();
        });
        assert!(matches!(
            published_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        release_tx.send(()).unwrap();
        entry.join().unwrap();
        publisher.join().unwrap();
        assert_eq!(ingress.phase(), PHASE_OPEN);
    }

    #[test]
    fn seal_retries_when_a_closing_entry_races_zero_active() {
        let ingress = Arc::new(ExportIngress::new());
        ingress.begin_opening();
        ingress.begin_close_with(|| {});

        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let sealing = Arc::clone(&ingress);
        let worker = std::thread::spawn(move || {
            sealing.seal_and_drain_with_hook(|| {
                ready_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
        });

        ready_rx.recv().unwrap();
        let (guard, accepted) = ingress.enter_with(|| {});
        assert!(!accepted);
        assert_eq!(ingress.active_calls(), 1);
        release_tx.send(()).unwrap();
        drop(guard);

        worker.join().unwrap();
        assert_eq!(ingress.phase(), PHASE_CLOSED);
        assert_eq!(ingress.active_calls(), 0);
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[test]
    // This abstract model checks epoch invariants; the concrete race above
    // exercises the AtomicU64/CAS implementation directly.
    fn loom_models_seal_and_reopen_linearization() {
        loom::model(|| {
            use loom::sync::{Arc, Mutex};

            #[derive(Clone, Copy)]
            struct ModelState {
                phase: u8,
                epoch: u64,
                active: usize,
            }

            let state = Arc::new(Mutex::new(ModelState {
                phase: PHASE_OPEN,
                epoch: 1,
                active: 0,
            }));
            let closing = Arc::clone(&state);
            let closer = loom::thread::spawn(move || {
                let mut state = closing.lock().unwrap();
                state.phase = PHASE_CLOSING;
                if state.active == 0 {
                    state.phase = PHASE_CLOSED;
                }
            });
            let entering = Arc::clone(&state);
            let caller = loom::thread::spawn(move || {
                let mut state = entering.lock().unwrap();
                if state.phase != PHASE_CLOSED {
                    state.active += 1;
                    let accepted_epoch = (state.phase == PHASE_OPEN).then_some(state.epoch);
                    state.active -= 1;
                    accepted_epoch
                } else {
                    None
                }
            });
            closer.join().unwrap();
            let accepted_epoch = caller.join().unwrap();
            let mut state = state.lock().unwrap();
            if state.active == 0 {
                state.phase = PHASE_CLOSED;
            }
            assert_eq!(state.active, 0);
            if let Some(epoch) = accepted_epoch {
                assert_eq!(epoch, 1);
            }
            state.epoch += 1;
            state.phase = PHASE_OPEN;
            assert_eq!(state.epoch, 2);
        });
    }
}
