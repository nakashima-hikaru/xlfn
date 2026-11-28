#[cfg(test)]
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
#[cfg(test)]
use std::thread::ThreadId;
#[cfg(test)]
use std::time::Duration;

use crossbeam_utils::CachePadded;
use xlfn_kernel::drain_gate::{
    DEFAULT_STRIPE_COUNT, StripedDrainGate, StripedDrainPermit, current_thread_stripe,
};

pub(crate) const PHASE_OPENING: u8 = 0;
pub(crate) const PHASE_OPEN: u8 = 1;
pub(crate) const PHASE_CLOSING: u8 = 2;
pub(crate) const PHASE_CLOSED: u8 = 3;

const INGRESS_STRIPE_COUNT: usize = DEFAULT_STRIPE_COUNT;

#[cfg(test)]
std::thread_local! {
    static DRAIN_PREPARATION_FAULT: std::cell::Cell<Option<(usize, usize)>> = const { std::cell::Cell::new(None) };
}

/// Injects a preparation failure before one of the two terminal waits. The
/// thread-local scope avoids affecting another runtime's concurrent fixture.
#[cfg(test)]
pub(crate) struct DrainPreparationFault;

#[cfg(test)]
impl DrainPreparationFault {
    pub(crate) fn at(wait: usize) -> Self {
        assert!(matches!(wait, 1 | 2));
        DRAIN_PREPARATION_FAULT.with(|fault| {
            assert!(fault.replace(Some((wait, 0))).is_none());
        });
        Self
    }

    pub(crate) fn attempts(&self) -> usize {
        DRAIN_PREPARATION_FAULT.with(|fault| fault.get().expect("active drain fault").1)
    }
}

#[cfg(test)]
impl Drop for DrainPreparationFault {
    fn drop(&mut self) {
        DRAIN_PREPARATION_FAULT.with(|fault| fault.set(None));
    }
}

#[inline]
fn current_ingress_stripe() -> usize {
    current_thread_stripe()
}

thread_local! {
    static ACTIVE_EXPORTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn active_export_on_current_thread() -> bool {
    ACTIVE_EXPORTS.with(|depth| depth.get() > 0)
}

fn enter_current_thread_export() {
    ACTIVE_EXPORTS.with(|depth| {
        let next = depth
            .get()
            .checked_add(1)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        depth.set(next);
    });
}

fn leave_current_thread_export() {
    ACTIVE_EXPORTS.with(|depth| {
        let next = xlfn_kernel::invariant::checked_sub_or_abort(depth.get(), 1);
        depth.set(next);
    });
}

/// `!Send` scope guard recording that the calling thread is inside a
/// synchronous FFI export boundary, even if it has no `Fp12CallGuard`.
pub(crate) struct ThreadExportGuard(std::marker::PhantomData<*const ()>);

impl ThreadExportGuard {
    pub(crate) fn enter() -> Self {
        enter_current_thread_export();
        Self(std::marker::PhantomData)
    }
}

impl Drop for ThreadExportGuard {
    fn drop(&mut self) {
        leave_current_thread_export();
    }
}

#[derive(Debug)]
struct UdfStripe {
    // Instrumentation only; StripedDrainGate owns admission and drain ordering.
    active: AtomicUsize,
}

impl UdfStripe {
    const fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
        }
    }

    fn enter(&self) {
        self.active
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |active| {
                active.checked_add(1)
            })
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
    }

    fn leave(&self) {
        self.active
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |active| {
                active.checked_sub(1)
            })
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
    }

    fn active(&self) -> usize {
        self.active.load(Ordering::Relaxed)
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
pub(crate) struct ExportsDrained {
    #[allow(
        dead_code,
        reason = "Linear proof token tracking epoch for ingress drain"
    )]
    epoch: u64,
}

/// Global ingress manager tracking all external DLL export calls entering the XLL.
///
/// Calls that arrive while OPENING or CLOSING are counted until their rejection
/// path has returned. Calls that arrive after the ingress is sealed CLOSED
/// cannot become active, which makes the drain certificate linearizable with
/// the CLOSED transition.
#[derive(Debug)]
pub(crate) struct ExportIngress {
    // The phase is global, while reservations are striped. Each stripe is
    // independently sealed by terminal drain, so ordinary UDF entry does not
    // contend on one cache line and close cannot overtake a reservation that
    // has already started.
    phase: AtomicU8,
    epoch: AtomicU64,
    exports: StripedDrainGate<INGRESS_STRIPE_COUNT>,
    udf_stripes: [CachePadded<UdfStripe>; INGRESS_STRIPE_COUNT],
    // Opening entries are rejected by the caller but counted until their
    // guards leave. Publication takes this lock so the zero-active check and
    // the final OPEN transition cannot be overtaken by a new entry.
    opening_lock: Mutex<()>,
    #[cfg(test)]
    test_epoch_active: AtomicUsize,
}

#[derive(Debug)]
pub(crate) struct OpeningPublicationLost;

/// Keeps rejected opening entries out between the quiescence check and OPEN.
pub(crate) struct OpeningPublication<'a> {
    ingress: &'a ExportIngress,
    _guard: std::sync::MutexGuard<'a, ()>,
}

impl OpeningPublication<'_> {
    pub(crate) fn publish(self, expected: &ExportIngress) -> Result<(), OpeningPublicationLost> {
        if !std::ptr::eq(self.ingress, expected) {
            return Err(OpeningPublicationLost);
        }
        self.ingress
            .phase
            .compare_exchange(
                PHASE_OPENING,
                PHASE_OPEN,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| OpeningPublicationLost)
    }
}

/// The result of reserving one external export entry.
///
/// Both variants own the counted ingress reservation.  Only `AdmittedExport`
/// can be passed to `Runtime::enter`; `RejectedExport` exists solely to keep a
/// closing/opening reservation alive until the caller has returned.
#[derive(Debug)]
pub(crate) enum ExportEntry<'a> {
    Admitted(AdmittedExport<'a>),
    Rejected(RejectedExport<'a>),
}

#[derive(Debug)]
pub(crate) struct AdmittedExport<'a> {
    guard: ExportCallGuard<'a>,
}

#[derive(Debug)]
pub(crate) struct RejectedExport<'a> {
    _guard: ExportCallGuard<'a>,
}

impl<'a> ExportEntry<'a> {
    pub(crate) fn into_admitted(self) -> Result<AdmittedExport<'a>, RejectedExport<'a>> {
        match self {
            Self::Admitted(entry) => Ok(entry),
            Self::Rejected(entry) => Err(entry),
        }
    }
}

impl AdmittedExport<'_> {
    pub(crate) fn assert_active(&self) {
        assert!(
            self.guard.stripe.is_some(),
            "admitted export must own an active ingress reservation"
        );
    }
}

#[cfg(any(test, feature = "refinement"))]
static DIAGNOSTIC_LINEARIZATION: Mutex<()> = Mutex::new(());

pub(crate) fn diagnostic_linearization() -> impl Drop {
    // A concrete guard in instrumented builds, a zero-sized scope otherwise.
    // Callers may release this scope before waiting for diagnostic readers.
    #[cfg(any(test, feature = "refinement"))]
    let guard = DIAGNOSTIC_LINEARIZATION
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    #[cfg(not(any(test, feature = "refinement")))]
    let guard = std::mem::DropGuard::new((), |()| {});
    guard
}

pub(crate) fn with_diagnostic_linearization<F, R>(operation: F) -> R
where
    F: FnOnce() -> R,
{
    // Only refinement observations need to order enqueue/flush with close.
    // Production queue ownership and shutdown are synchronized independently.
    let _guard = diagnostic_linearization();
    operation()
}

impl Default for ExportIngress {
    fn default() -> Self {
        Self::new()
    }
}

impl ExportIngress {
    pub(crate) const fn new() -> Self {
        Self {
            phase: AtomicU8::new(PHASE_CLOSED),
            epoch: AtomicU64::new(0),
            exports: StripedDrainGate::new_sealed(),
            udf_stripes: [const { CachePadded::new(UdfStripe::new()) }; INGRESS_STRIPE_COUNT],
            opening_lock: Mutex::new(()),
            #[cfg(test)]
            test_epoch_active: AtomicUsize::new(0),
        }
    }

    /// Starts a new ingress epoch in the non-admitting opening phase.
    pub(crate) fn begin_opening(&self) {
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
        drop(
            self.exports
                .control()
                .wait_if_sealed()
                .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop())
                .reopen(),
        );
        self.phase.store(PHASE_OPENING, Ordering::Release);
    }

    /// Takes the opening lock and waits out all earlier rejected entries.
    ///
    /// The returned owner keeps external entry rejected until publication.
    /// An opening entry cannot race the zero-active observation because it
    /// must acquire the same opening lock first.
    pub(crate) fn opening_publication(
        &self,
    ) -> Result<OpeningPublication<'_>, OpeningPublicationLost> {
        if crate::return_abi::numeric::Fp12CallGuard::on_current_thread()
            || active_export_on_current_thread()
        {
            return Err(OpeningPublicationLost);
        }
        let guard = self
            .opening_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());

        self.wait_for_quiescence();

        if self.phase.load(Ordering::Acquire) != PHASE_OPENING {
            return Err(OpeningPublicationLost);
        }

        Ok(OpeningPublication {
            ingress: self,
            _guard: guard,
        })
    }

    #[cfg(test)]
    pub(crate) fn complete_open<F, E>(
        &self,
        operation: F,
    ) -> Result<Result<(), E>, OpeningPublicationLost>
    where
        F: FnOnce() -> Result<(), E>,
    {
        let publication = self.opening_publication()?;
        let result = operation();
        if result.is_ok() {
            publication.publish(self)?;
        }
        Ok(result)
    }

    /// Attempts to enter an export entry and runs `on_accepted` after the
    /// accepting state transition has reserved its striped counter.
    pub(crate) fn enter_with<F>(&self, on_accepted: F) -> ExportEntry<'_>
    where
        F: FnOnce(),
    {
        match self.enter_with_result(on_accepted) {
            Ok((entry, ())) => ExportEntry::Admitted(entry),
            Err(entry) => ExportEntry::Rejected(entry),
        }
    }

    /// Returns the callback's result with the admitted export. The callback
    /// runs in the accepting scope, before releasing any opening lock taken
    /// when entry first observed an opening epoch.
    pub(crate) fn enter_with_result<T>(
        &self,
        on_accepted: impl FnOnce() -> T,
    ) -> Result<(AdmittedExport<'_>, T), RejectedExport<'_>> {
        self.enter_observed(self.phase.load(Ordering::Acquire), false, on_accepted)
    }

    /// Attempts to enter a UDF export entry and runs `on_accepted` after the
    /// accepting state transition has reserved its striped counter.
    pub(crate) fn enter_udf_with<F>(&self, on_accepted: F) -> ExportEntry<'_>
    where
        F: FnOnce(),
    {
        match self.enter_observed(self.phase.load(Ordering::Acquire), true, on_accepted) {
            Ok((entry, ())) => ExportEntry::Admitted(entry),
            Err(entry) => ExportEntry::Rejected(entry),
        }
    }

    fn enter_observed<T>(
        &self,
        observed_phase: u8,
        udf: bool,
        on_accepted: impl FnOnce() -> T,
    ) -> Result<(AdmittedExport<'_>, T), RejectedExport<'_>> {
        if observed_phase == PHASE_CLOSED {
            return Err(self.rejected_guard());
        }
        let opening_guard = (observed_phase == PHASE_OPENING).then(|| {
            self.opening_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner())
        });
        let stripe_index = current_ingress_stripe();
        let Ok(permit) = self.exports.try_enter(stripe_index) else {
            return Err(self.rejected_guard());
        };
        // A late reservation from a previous epoch must not create a live
        // opening guard outside the publication lock's quiescence check.
        let phase = self.phase.load(Ordering::Acquire);
        if phase == PHASE_CLOSED || (phase == PHASE_OPENING && opening_guard.is_none()) {
            drop(permit);
            return Err(self.rejected_guard());
        }
        if udf {
            self.udf_stripes[stripe_index].enter();
        }
        let entry = ExportCallGuard {
            ingress: self,
            epoch: self.epoch.load(Ordering::Acquire),
            stripe: Some(stripe_index),
            _permit: Some(permit),
            udf,
        };
        if phase == PHASE_OPEN {
            let result = on_accepted();
            Ok((AdmittedExport { guard: entry }, result))
        } else {
            Err(RejectedExport { _guard: entry })
        }
    }

    /// Stops accepting new export calls and runs `on_closed` after the
    /// operational closing transition succeeds.
    pub(crate) fn begin_close_with<F>(&self, on_closed: F)
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
        // Close is a cold path. Always serialize it with opening publication:
        // an initial OPEN/CLOSED observation can become stale across an epoch
        // rollover before the later compare-exchange observes OPENING.
        let _opening_guard = self
            .opening_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        with_diagnostic_linearization(|| {
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
                        on_closed();
                        return;
                    }
                    Err(current) => observed = current,
                }
            }
        });
    }

    fn wait_for_quiescence(&self) {
        drop(self.exports.control().wait_idle());
    }

    fn drain_exports(&self, wait: impl FnOnce() + Send) -> crate::XllResult<()> {
        #[cfg(test)]
        if DRAIN_PREPARATION_FAULT.with(|fault| {
            let Some((target, attempts)) = fault.get() else {
                return false;
            };
            let attempts = attempts + 1;
            fault.set(Some((target, attempts)));
            attempts == target
        }) {
            return Err(crate::XllError::native(
                -1,
                "injected export drain preparation failure",
            ));
        }
        if crate::return_abi::numeric::Fp12CallGuard::on_current_thread()
            || active_export_on_current_thread()
        {
            return Err(crate::XllError::native(
                -1,
                "export drain would wait for the calling thread's active export",
            ));
        }
        #[cfg(windows)]
        {
            use crate::drain_wait::{UnstartedDrain, drain_with_com_dispatch};

            let prepared = drain_with_com_dispatch(UnstartedDrain::new(wait))
                .map_err(|(error, _unstarted)| error)?;
            let joined = prepared.run().map_err(|(error, _unstarted)| error)?;
            joined.into_output();
            Ok(())
        }
        #[cfg(not(windows))]
        {
            wait();
            Ok(())
        }
    }

    /// Waits for the current epoch to drain and seals it CLOSED in the same
    /// synchronization region that observes `active == 0`.
    pub(crate) fn seal_and_drain(&self) -> crate::XllResult<ExportsDrained> {
        self.seal_and_drain_with_hook(|| {})
    }

    fn seal_and_drain_with_hook<F>(&self, before_close: F) -> crate::XllResult<ExportsDrained>
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
        self.drain_exports(|| drop(self.exports.control().wait_idle()))?;

        if let Some(before_close) = before_close.take() {
            before_close();
        }

        // Closing entries may have started after the zero observation. Seal
        // every stripe to prevent a late reservation from being missed by the
        // terminal CLOSED transition, then drain those reservations.
        self.drain_exports(|| drop(self.exports.control().seal().wait()))?;

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
        Ok(ExportsDrained {
            epoch: self.epoch.load(Ordering::Acquire),
        })
    }

    pub(crate) fn phase(&self) -> u8 {
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

    pub(crate) fn active_calls(&self) -> usize {
        self.exports.active()
    }

    pub(crate) fn active_udfs(&self) -> usize {
        self.udf_stripes.iter().map(|stripe| stripe.active()).sum()
    }

    fn rejected_guard(&self) -> RejectedExport<'_> {
        RejectedExport {
            _guard: ExportCallGuard {
                ingress: self,
                epoch: self.epoch.load(Ordering::Acquire),
                stripe: None,
                _permit: None,
                udf: false,
            },
        }
    }
}

#[derive(Debug)]
pub(crate) struct ExportCallGuard<'a> {
    ingress: &'a ExportIngress,
    epoch: u64,
    stripe: Option<usize>,
    _permit: Option<StripedDrainPermit<'a, INGRESS_STRIPE_COUNT>>,
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
        if self.udf {
            self.ingress.udf_stripes[stripe_index].leave();
        }
        let _ = self._permit.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, mpsc};

    #[test]
    fn open_and_close_refuse_to_wait_for_an_export_on_their_own_thread() {
        let ingress = ExportIngress::new();
        ingress.begin_opening();
        {
            let _sta_udf = crate::return_abi::numeric::Fp12CallGuard::enter();
            let rejected = ingress.enter_with(|| {});
            assert!(ingress.opening_publication().is_err());
            drop(rejected);
        }
        {
            let _non_fp12_scope = ThreadExportGuard::enter();
            let non_fp12 = ingress.enter_with(|| {});
            assert!(ingress.opening_publication().is_err());
            drop(non_fp12);
        }
        ingress.complete_open(|| Ok::<(), ()>(())).unwrap().unwrap();
        let sta_udf = crate::return_abi::numeric::Fp12CallGuard::enter();
        let admitted = ingress.enter_with(|| {});
        ingress.begin_close_with(|| {});
        assert!(matches!(
            (ingress.seal_and_drain())
                .as_ref()
                .map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::Native { .. })
        ));
        assert_eq!(ingress.phase(), PHASE_CLOSING);
        drop(admitted);
        drop(sta_udf);
        assert!(ingress.seal_and_drain().is_ok());
        assert_eq!(ingress.phase(), PHASE_CLOSED);

        let second = ExportIngress::new();
        second.begin_opening();
        second.complete_open(|| Ok::<(), ()>(())).unwrap().unwrap();
        let non_fp12_scope = ThreadExportGuard::enter();
        let non_fp12_admitted = second.enter_with(|| {});
        second.begin_close_with(|| {});
        assert!(matches!(
            (second.seal_and_drain())
                .as_ref()
                .map_err(crate::error::construct::view),
            Err(crate::error::construct::ErrorView::Native { .. })
        ));
        drop(non_fp12_admitted);
        drop(non_fp12_scope);
        assert!(second.seal_and_drain().is_ok());
    }

    #[test]
    fn seal_is_linearized_with_the_last_active_guard() {
        let ingress = Arc::new(ExportIngress::new());
        ingress.begin_opening();
        ingress.begin_close_with(|| {});
        let entry = ingress.enter_with(|| {});
        assert!(matches!(&entry, ExportEntry::Rejected(_)));

        let sealing = Arc::clone(&ingress);
        let worker = std::thread::spawn(move || sealing.seal_and_drain());
        std::thread::yield_now();
        assert_eq!(ingress.active_calls(), 1);
        drop(entry);
        let certificate = worker.join().unwrap().unwrap();

        assert_eq!(certificate.epoch, 1);
        assert_eq!(ingress.phase(), PHASE_CLOSED);
        assert_eq!(ingress.active_calls(), 0);
        let closed_entry = ingress.enter_with(|| {});
        assert!(matches!(&closed_entry, ExportEntry::Rejected(_)));
        assert_eq!(ingress.active_calls(), 0);
        drop(closed_entry);
    }

    #[test]
    fn a_stale_phase_observation_cannot_reserve_an_opening_export() {
        let ingress = ExportIngress::new();
        let closed = ingress.phase();
        ingress.begin_opening();
        for udf in [false, true] {
            for sampled in [closed, PHASE_CLOSING] {
                let rejected = ingress
                    .enter_observed(sampled, udf, || panic!("opening export accepted"))
                    .unwrap_err();
                assert_eq!(ingress.active_calls(), 0);
                assert_eq!(ingress.active_udfs(), 0);
                drop(rejected);
            }
        }
        ingress.complete_open(|| Ok::<_, ()>(())).unwrap().unwrap();
        assert!(ingress.enter_with_result(|| ()).is_ok());
    }

    #[test]
    fn close_serializes_with_epoch_publication_from_every_initial_phase() {
        for open in [false, true] {
            let ingress = ExportIngress::new();
            if open {
                ingress.begin_opening();
                ingress.complete_open(|| Ok::<_, ()>(())).unwrap().unwrap();
            }
            let publication = ingress.opening_lock.lock().unwrap();
            std::thread::scope(|scope| {
                let (attempted_tx, attempted_rx) = mpsc::channel();
                let (finished_tx, finished_rx) = mpsc::channel();
                let ingress = &ingress;
                let closing = scope.spawn(move || {
                    ingress.begin_close_with_attempt_hook(|| attempted_tx.send(()).unwrap(), || {});
                    finished_tx.send(()).unwrap();
                });
                attempted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                // Even OPEN/CLOSED is just a sampled projection. The epoch
                // publication guard, rather than that sample, orders close.
                let premature = finished_rx.recv_timeout(Duration::from_millis(20)).is_ok();
                drop(publication);
                closing.join().unwrap();
                assert!(!premature, "close bypassed the epoch publication guard");
            });
            assert_eq!(
                ingress.phase(),
                if open { PHASE_CLOSING } else { PHASE_CLOSED }
            );
        }
    }

    #[test]
    fn drain_rechecks_quiescence_without_exit_notification() {
        let ingress = ExportIngress::new();

        ingress.begin_opening();
        ingress.complete_open(|| Ok::<_, ()>(())).unwrap().unwrap();

        let entry = ingress.enter_with(|| {});
        assert!(matches!(&entry, ExportEntry::Admitted(_)));

        ingress.begin_close_with(|| {});

        std::thread::scope(|scope| {
            let drain = scope.spawn(|| ingress.seal_and_drain());

            std::thread::sleep(Duration::from_millis(10));
            drop(entry);

            let _drained = drain.join().unwrap().unwrap();
        });

        assert_eq!(ingress.phase(), PHASE_CLOSED);
    }

    #[test]
    fn close_progresses_while_accepted_entry_observation_is_paused() {
        let ingress = Arc::new(ExportIngress::new());
        ingress.begin_opening();
        ingress.complete_open(|| Ok::<(), ()>(())).unwrap().unwrap();
        let (hook_started_tx, hook_started_rx) = mpsc::sync_channel(1);
        let (release_hook_tx, release_hook_rx) = mpsc::sync_channel(1);
        let entering = Arc::clone(&ingress);
        let entry = std::thread::spawn(move || {
            let (entry, result) = entering
                .enter_with_result(|| {
                    hook_started_tx.send(()).unwrap();
                    release_hook_rx.recv().unwrap();
                    17
                })
                .unwrap();
            assert_eq!(result, 17);
            assert_eq!(entering.active_calls(), 1);
            drop(entry);
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
        closed_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("close must not wait for an observation hook");
        assert_eq!(ingress.phase(), PHASE_CLOSING);

        release_hook_tx.send(()).unwrap();
        entry.join().unwrap();
        close.join().unwrap();
        assert_eq!(ingress.phase(), PHASE_CLOSING);
    }

    #[test]
    fn miri_result_admission_does_not_run_a_rejected_operation_and_releases_on_unwind() {
        let ingress = ExportIngress::new();
        ingress.begin_opening();
        let rejected = ingress
            .enter_with_result(|| panic!("opening must reject admission"))
            .unwrap_err();
        assert_eq!(ingress.active_calls(), 1);
        drop(rejected);
        ingress.complete_open(|| Ok::<_, ()>(())).unwrap().unwrap();

        let interrupted =
            crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = ingress.enter_with_result(|| {
                    assert_eq!(ingress.active_calls(), 1);
                    panic!("injected unwind inside admission callback");
                });
            }));
        assert!(interrupted.is_err());
        assert_eq!(ingress.active_calls(), 0);
        ingress.begin_close_with(|| {});
        ingress.seal_and_drain().unwrap();
    }

    #[test]
    fn opening_starts_a_distinct_epoch_only_after_seal() {
        let ingress = ExportIngress::new();
        ingress.begin_opening();
        ingress.begin_close_with(|| {});
        let first = ingress.seal_and_drain().unwrap();
        ingress.begin_opening();
        ingress.complete_open(|| Ok::<(), ()>(())).unwrap().unwrap();
        let entry = ingress.enter_with(|| {});
        assert!(matches!(&entry, ExportEntry::Admitted(_)));
        drop(entry);
        ingress.begin_close_with(|| {});
        let second = ingress.seal_and_drain().unwrap();
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
            let entry = entering.enter_with(|| {});
            assert!(matches!(&entry, ExportEntry::Rejected(_)));
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(entry);
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
        let entry = ingress.enter_with(|| {});
        assert!(matches!(&entry, ExportEntry::Rejected(_)));
        assert_eq!(ingress.active_calls(), 1);
        release_tx.send(()).unwrap();
        drop(entry);

        worker.join().unwrap().unwrap();
        assert_eq!(ingress.phase(), PHASE_CLOSED);
        assert_eq!(ingress.active_calls(), 0);
    }
}
