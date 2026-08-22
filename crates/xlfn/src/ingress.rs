use std::cell::Cell;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
#[cfg(test)]
use std::thread::ThreadId;
use std::time::Duration;

pub const PHASE_OPENING: u8 = 0;
pub const PHASE_OPEN: u8 = 1;
pub const PHASE_CLOSING: u8 = 2;
pub const PHASE_CLOSED: u8 = 3;

const INGRESS_STRIPE_COUNT: usize = 32;
const STRIPE_SEALED: usize = 1_usize << (usize::BITS - 1);
const STRIPE_COUNT_MASK: usize = STRIPE_SEALED - 1;
const QUIESCENCE_RECHECK_INTERVAL: Duration = Duration::from_millis(1);

thread_local! {
    static INGRESS_STRIPE: Cell<usize> = const { Cell::new(usize::MAX) };
}

fn current_ingress_stripe() -> usize {
    let current = INGRESS_STRIPE.get();
    if current != usize::MAX {
        return current;
    }
    let assigned = NEXT_INGRESS_STRIPE.fetch_add(1, Ordering::Relaxed) & (INGRESS_STRIPE_COUNT - 1);
    INGRESS_STRIPE.set(assigned);
    assigned
}

static NEXT_INGRESS_STRIPE: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
#[repr(C, align(128))]
struct IngressStripe {
    // The high bit closes this stripe against new reservations during the
    // terminal drain. The low bits count every live external export.
    active: AtomicUsize,
    udf_active: AtomicUsize,
}

impl IngressStripe {
    const fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            udf_active: AtomicUsize::new(0),
        }
    }

    fn try_enter(&self) -> bool {
        self.active
            .try_update(Ordering::Acquire, Ordering::Relaxed, |state| {
                if state & STRIPE_SEALED != 0 {
                    return None;
                }
                if state & STRIPE_COUNT_MASK == STRIPE_COUNT_MASK {
                    std::process::abort();
                }
                Some(state + 1)
            })
            .is_ok()
    }

    fn enter_udf(&self) {
        self.udf_active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_add(1)
            })
            .unwrap_or_else(|_| std::process::abort());
    }

    fn leave_udf(&self) {
        self.udf_active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .unwrap_or_else(|_| std::process::abort());
    }

    fn leave(&self) {
        self.active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                let active = state & STRIPE_COUNT_MASK;
                active
                    .checked_sub(1)
                    .map(|next| (state & STRIPE_SEALED) | next)
            })
            .unwrap_or_else(|_| std::process::abort());
    }

    fn seal(&self) {
        let _ = self
            .active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                if state & STRIPE_SEALED != 0 {
                    None
                } else {
                    Some(state | STRIPE_SEALED)
                }
            });
    }

    fn reopen(&self) {
        debug_assert_eq!(self.active.load(Ordering::Acquire) & STRIPE_COUNT_MASK, 0);
        debug_assert_eq!(self.udf_active.load(Ordering::Acquire), 0);
        self.active.store(0, Ordering::Release);
    }

    fn active(&self) -> usize {
        self.active.load(Ordering::Acquire) & STRIPE_COUNT_MASK
    }

    fn active_udfs(&self) -> usize {
        self.udf_active.load(Ordering::Acquire)
    }
}

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
    // The phase is global, while reservations are striped. Each stripe is
    // independently sealed by terminal drain, so ordinary UDF entry does not
    // contend on one cache line and close cannot overtake a reservation that
    // has already started.
    phase: AtomicU8,
    epoch: AtomicU64,
    stripes: [IngressStripe; INGRESS_STRIPE_COUNT],
    // Used only by rare lifecycle quiescence waits.
    // Ordinary export/UDF entry and exit never acquire this mutex.
    wait_lock: Mutex<()>,
    idle: Condvar,
    // Opening entries are rejected by the caller but counted until their
    // guards leave. Publication takes this lock so the zero-active check and
    // the final OPEN transition cannot be overtaken by a new entry.
    opening_lock: Mutex<()>,
    #[cfg(any(test, feature = "unstable"))]
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

static DIAGNOSTIC_LINEARIZATION: Mutex<()> = Mutex::new(());

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
            phase: AtomicU8::new(PHASE_CLOSED),
            epoch: AtomicU64::new(0),
            stripes: [const { IngressStripe::new() }; INGRESS_STRIPE_COUNT],
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
            opening_lock: Mutex::new(()),
            #[cfg(any(test, feature = "unstable"))]
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
        let owns_test_epoch = std::ptr::eq(self, crate::module_runtime::ingress());
        #[cfg(test)]
        if owns_test_epoch {
            TEST_EPOCH_GATE.acquire();
            self.test_epoch_active.store(1, Ordering::Release);
        }
        let phase = self.phase.load(Ordering::Acquire);
        if phase != PHASE_CLOSED || self.active_calls() != 0 {
            #[cfg(test)]
            if owns_test_epoch {
                self.test_epoch_active.store(0, Ordering::Release);
                TEST_EPOCH_GATE.release();
            }
            assert_eq!(phase, PHASE_CLOSED, "ingress opening before seal");
            assert_eq!(
                self.active_calls(),
                0,
                "ingress opening with live export guards"
            );
        }
        self.epoch
            .try_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                epoch.checked_add(1)
            })
            .unwrap_or_else(|_| std::process::abort());
        for stripe in &self.stripes {
            stripe.reopen();
        }
        self.phase.store(PHASE_OPENING, Ordering::Release);
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
        #[cfg(any(test, feature = "unstable"))]
        let _linearization_guard = self
            .linearization_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        self.wait_for_quiescence();

        if self.phase.load(Ordering::Acquire) != PHASE_OPENING {
            return Err(OpeningPublicationLost);
        }

        let result = operation();
        if result.is_err() {
            return Ok(result);
        }
        match self.phase.compare_exchange(
            PHASE_OPENING,
            PHASE_OPEN,
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
        let observed_phase = self.phase.load(Ordering::Acquire);
        let _opening_guard = (observed_phase == PHASE_OPENING).then(|| {
            self.opening_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        });
        #[cfg(any(test, feature = "unstable"))]
        let _linearization_guard = self
            .linearization_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut on_accepted = Some(on_accepted);
        if self.phase.load(Ordering::Acquire) == PHASE_CLOSED {
            return (self.rejected_guard(), false);
        }
        let stripe_index = current_ingress_stripe();
        let stripe = &self.stripes[stripe_index];
        if !stripe.try_enter() {
            return (self.rejected_guard(), false);
        }
        let phase = self.phase.load(Ordering::Acquire);
        if phase == PHASE_CLOSED {
            self.release_reservation(stripe_index, false);
            return (self.rejected_guard(), false);
        }
        let accepted = phase == PHASE_OPEN;
        if accepted {
            let hook = on_accepted
                .take()
                .expect("ingress acceptance hook called once");
            hook();
        }
        (
            ExportCallGuard {
                ingress: self,
                epoch: self.epoch.load(Ordering::Acquire),
                stripe: Some(stripe_index),
                udf: false,
            },
            accepted,
        )
    }

    /// Attempts to enter a UDF export entry and runs `on_accepted` at the same
    /// refinement linearization point as the accepting state transition.
    pub fn enter_udf_with<F>(&self, on_accepted: F) -> (ExportCallGuard<'_>, bool)
    where
        F: FnOnce(),
    {
        let observed_phase = self.phase.load(Ordering::Acquire);
        let _opening_guard = (observed_phase == PHASE_OPENING).then(|| {
            self.opening_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        });
        #[cfg(any(test, feature = "unstable"))]
        let _linearization_guard = self
            .linearization_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut on_accepted = Some(on_accepted);
        if self.phase.load(Ordering::Acquire) == PHASE_CLOSED {
            return (self.rejected_guard(), false);
        }
        let stripe_index = current_ingress_stripe();
        let stripe = &self.stripes[stripe_index];
        if !stripe.try_enter() {
            return (self.rejected_guard(), false);
        }
        stripe.enter_udf();
        let phase = self.phase.load(Ordering::Acquire);
        if phase == PHASE_CLOSED {
            self.release_reservation(stripe_index, true);
            return (self.rejected_guard(), false);
        }
        let accepted = phase == PHASE_OPEN;
        if accepted {
            let hook = on_accepted
                .take()
                .expect("ingress acceptance hook called once");
            hook();
        }
        (
            ExportCallGuard {
                ingress: self,
                epoch: self.epoch.load(Ordering::Acquire),
                stripe: Some(stripe_index),
                udf: true,
            },
            accepted,
        )
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
        let observed_phase = self.phase.load(Ordering::Acquire);
        let _opening_guard = (observed_phase == PHASE_OPENING).then(|| {
            self.opening_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        });
        #[cfg(any(test, feature = "unstable"))]
        let _linearization_guard = self
            .linearization_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        #[cfg(test)]
        self.close_waiters.fetch_sub(1, Ordering::AcqRel);
        with_diagnostic_linearization(|| {
            let mut on_closed = Some(on_closed);
            let mut observed = self.phase.load(Ordering::Acquire);
            loop {
                if !matches!(observed, PHASE_OPEN | PHASE_OPENING) {
                    return;
                }
                match self.phase.compare_exchange_weak(
                    observed,
                    PHASE_CLOSING,
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
        #[cfg(any(test, feature = "unstable"))]
        let _linearization_guard = self
            .linearization_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        operation()
    }

    fn wait_for_quiescence(&self) {
        let mut guard = self
            .wait_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        while self.active_calls() != 0 {
            let (next_guard, _) = self
                .idle
                .wait_timeout(guard, QUIESCENCE_RECHECK_INTERVAL)
                .unwrap_or_else(|error| error.into_inner());

            guard = next_guard;
        }
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
                self.phase.load(Ordering::Acquire),
                PHASE_OPEN | PHASE_OPENING
            ),
            "ingress sealed before begin_close"
        );
        let mut before_close = Some(before_close);
        self.wait_for_quiescence();

        if let Some(before_close) = before_close.take() {
            before_close();
        }

        // Closing entries may have started after the zero observation. Seal
        // every stripe to prevent a late reservation from being missed by the
        // terminal CLOSED transition, then drain those reservations.
        for stripe in &self.stripes {
            stripe.seal();
        }
        self.wait_for_quiescence();

        match self.phase.compare_exchange(
            PHASE_CLOSING,
            PHASE_CLOSED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) | Err(PHASE_CLOSED) => {}
            Err(PHASE_OPEN | PHASE_OPENING) => panic!("ingress sealed before begin_close"),
            Err(_) => std::process::abort(),
        }
        #[cfg(test)]
        if std::ptr::eq(self, crate::module_runtime::ingress())
            && self.test_epoch_active.swap(0, Ordering::AcqRel) != 0
        {
            TEST_EPOCH_GATE.release();
        }
        ExportsDrained {
            epoch: self.epoch.load(Ordering::Acquire),
        }
    }

    pub fn phase(&self) -> u8 {
        self.phase.load(Ordering::Acquire)
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub(crate) fn allows_diagnostic_mutation(&self) -> bool {
        let phase = self.phase.load(Ordering::Acquire);
        phase == PHASE_OPENING
            || phase == PHASE_OPEN
            || (phase == PHASE_CLOSED && self.epoch.load(Ordering::Acquire) == 0)
    }

    pub fn active_calls(&self) -> usize {
        self.stripes.iter().map(IngressStripe::active).sum()
    }

    pub fn active_udfs(&self) -> usize {
        self.stripes.iter().map(IngressStripe::active_udfs).sum()
    }

    fn rejected_guard(&self) -> ExportCallGuard<'_> {
        ExportCallGuard {
            ingress: self,
            epoch: self.epoch.load(Ordering::Acquire),
            stripe: None,
            udf: false,
        }
    }

    fn release_reservation(&self, stripe_index: usize, udf: bool) {
        let stripe = &self.stripes[stripe_index];

        if udf {
            stripe.leave_udf();
        }

        stripe.leave();
    }
}

pub struct ExportCallGuard<'a> {
    ingress: &'a ExportIngress,
    epoch: u64,
    stripe: Option<usize>,
    udf: bool,
}

impl Drop for ExportCallGuard<'_> {
    fn drop(&mut self) {
        let Some(stripe_index) = self.stripe else {
            return;
        };
        assert_eq!(
            self.ingress.epoch.load(Ordering::Acquire),
            self.epoch,
            "export guard crossed ingress epochs"
        );
        self.ingress.release_reservation(stripe_index, self.udf);
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
    fn drain_rechecks_quiescence_without_exit_notification() {
        let ingress = ExportIngress::new();

        ingress.begin_opening();
        ingress.complete_open(|| Ok::<_, ()>(())).unwrap().unwrap();

        let (guard, accepted) = ingress.enter_with(|| {});
        assert!(accepted);

        ingress.begin_close_with(|| {});

        std::thread::scope(|scope| {
            let drain = scope.spawn(|| ingress.seal_and_drain());

            std::thread::sleep(Duration::from_millis(10));
            drop(guard);

            let _drained = drain.join().unwrap();
        });

        assert_eq!(ingress.phase(), PHASE_CLOSED);
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
