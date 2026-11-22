use crate::panic_boundary::catch_no_unwind;
use crate::{XllError, XllResult};
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;
#[cfg(test)]
use std::panic::catch_unwind;
use std::time::{Duration, Instant, SystemTime};

/// Identifier of one worksheet-function invocation within the runtime.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CallId(u64);

impl CallId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    /// Returns the numeric identifier for logging and correlation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Identifier of the runtime calculation generation that admitted a call.
///
/// Advances on Excel's registered calculation-ended events. Excel may omit
/// those events for some programmatic recalculations, so this is not a unique
/// identifier for every possible recalculation request.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CalculationId(u64);

impl CalculationId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    /// Returns the numeric identifier for logging and correlation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
/// Invocation metadata supplied to [`UdfLayer::enter`].
pub struct CallMetadata {
    /// Stable Rust UDF identity recorded by registration.
    pub udf_id: &'static str,
    /// Excel-visible registered function name.
    pub excel_name: &'static str,
    /// Identifier of this invocation.
    pub call_id: CallId,
    /// Calculation generation that admitted the invocation.
    pub calculation_id: CalculationId,
    /// Wall-clock time when execution instrumentation began.
    pub started_at: SystemTime,
    /// Number of admitted calls active when metadata was collected.
    pub concurrent_calls: usize,
}

pub(crate) const UDF_TRACE_TARGET: &str = "xlfn::udf";

pub(crate) fn udf_trace_enabled() -> bool {
    // Both global and thread-scoped dispatchers set this flag. Before either
    // has been installed there is no subscriber to query, including via TLS.
    if !tracing::dispatcher::has_been_set() {
        return false;
    }
    catch_no_unwind(AssertUnwindSafe(
        || tracing::enabled!(target: UDF_TRACE_TARGET, tracing::Level::INFO),
    ))
    .unwrap_or(false)
}

pub(crate) struct InstrumentationPlan<A: crate::Addin> {
    trace_enabled: bool,
    _marker: PhantomData<fn() -> A>,
}

impl<A: crate::Addin> InstrumentationPlan<A> {
    pub(crate) fn for_call(_call: &crate::runtime::CallGuard<'_, A>) -> Self {
        Self {
            trace_enabled: udf_trace_enabled(),
            _marker: PhantomData,
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        layers_enabled::<A::Layers>() || self.trace_enabled
    }

    /// Starts observation only on the observed path. Capture may acquire a
    /// calculation snapshot; it runs before any user layer is entered.
    pub(crate) fn start<C>(
        &self,
        udf_id: &'static str,
        excel_name: &'static str,
        call_id: CallId,
        capture: impl FnOnce() -> (CalculationId, C),
    ) -> (ObservationEntry, C) {
        let timer = CallTimer::start();
        let (calculation_id, captured) = capture();
        let entry = ObservationEntry {
            timer,
            trace_metadata: UdfTraceMetadata {
                udf_id,
                excel_name,
                call_id,
                calculation_id,
                concurrent_calls: crate::module_runtime::ingress().active_udfs(),
            },
            trace_enabled: self.trace_enabled,
        };
        (entry, captured)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct UdfTraceMetadata {
    pub udf_id: &'static str,
    pub excel_name: &'static str,
    pub call_id: CallId,
    pub calculation_id: CalculationId,
    pub concurrent_calls: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CallTimer {
    started: Instant,
}

impl CallTimer {
    pub(crate) fn start() -> Self {
        Self {
            started: Instant::now(),
        }
    }

    pub(crate) fn elapsed(self) -> Duration {
        self.started.elapsed()
    }
}

/// Timed observation before layer admission. Its rejection factory finalizes
/// the boundary-specific outcome before any partially entered layer exits.
pub(crate) struct ObservationEntry {
    timer: CallTimer,
    trace_metadata: UdfTraceMetadata,
    trace_enabled: bool,
}

impl ObservationEntry {
    pub(crate) fn enter<L: UdfLayers, R: LayerRejection>(
        self,
        layers: &L,
        reject: impl FnOnce(XllError, CallTimer) -> R,
    ) -> Result<CallObservation<<L as private::UdfLayersImpl>::Guards>, RejectedObservation<R>>
    {
        let layers = if layers_enabled::<L>() {
            let UdfTraceMetadata {
                udf_id,
                excel_name,
                call_id,
                calculation_id,
                concurrent_calls,
            } = self.trace_metadata;
            let metadata = CallMetadata {
                udf_id,
                excel_name,
                call_id,
                calculation_id,
                started_at: SystemTime::now(),
                concurrent_calls,
            };
            match enter_layers(layers, &metadata, |error| reject(error, self.timer)) {
                Ok(layers) => Some(layers),
                Err(rejection) => {
                    return Err(RejectedObservation {
                        entry: self,
                        rejection,
                    });
                }
            }
        } else {
            None
        };
        Ok(CallObservation {
            entry: self,
            layers,
        })
    }

    fn trace(&self, outcome: &CallOutcome<'_>) {
        if self.trace_enabled {
            trace(&self.trace_metadata, outcome);
        }
    }
}

/// Layer rejection has already produced a boundary result and exited all
/// prior guards. Report and trace that same result before returning it.
pub(crate) struct RejectedObservation<R> {
    entry: ObservationEntry,
    rejection: R,
}

impl<R: LayerRejection> RejectedObservation<R> {
    pub(crate) fn finish(self, report: impl FnOnce(&R)) -> R {
        report(&self.rejection);
        if self.entry.trace_enabled {
            self.entry.trace(&self.rejection.outcome());
        }
        self.rejection
    }
}

/// Owns the admitted layer guards and emits one shared outcome, running
/// each guard's exit behind its own panic boundary.
pub(crate) struct CallObservation<G: UdfLayerGuard> {
    entry: ObservationEntry,
    layers: Option<G>,
}

impl<G: UdfLayerGuard> CallObservation<G> {
    #[cfg(all(test, feature = "async"))]
    pub(crate) fn for_test(
        metadata: &CallMetadata,
        timer: CallTimer,
        layers: Option<G>,
        trace_enabled: bool,
    ) -> Self {
        Self {
            entry: ObservationEntry {
                timer,
                trace_metadata: UdfTraceMetadata {
                    udf_id: metadata.udf_id,
                    excel_name: metadata.excel_name,
                    call_id: metadata.call_id,
                    calculation_id: metadata.calculation_id,
                    concurrent_calls: metadata.concurrent_calls,
                },
                trace_enabled,
            },
            layers,
        }
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.entry.timer.elapsed()
    }

    pub(crate) fn finish(self, outcome: &CallOutcome<'_>) {
        if let Some(layers) = self.layers {
            exit_layers(layers, outcome);
        }
        self.entry.trace(outcome);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
/// Broad classification of a failed worksheet invocation.
pub enum UdfErrorKind {
    /// An input could not be converted or validated.
    Input,
    /// The calculation rejected an application-domain value.
    Domain,
    /// An application or vendor error supplied a native error code.
    Vendor,
    /// The application intentionally returned an Excel worksheet error.
    ExcelValue,
    /// A panic was caught at the framework boundary.
    Panic,
    /// The runtime is closing and cannot admit the invocation.
    Closing,
    /// A framework or unclassified error occurred.
    Internal,
}

#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
/// Completion of the Rust calculation, independently of Excel delivery.
pub enum UdfCompletionOutcome<'call> {
    /// The Rust calculation completed successfully.
    Success,
    /// The calculation returned an error.
    Error {
        /// Broad classification of the error.
        kind: UdfErrorKind,
        /// Error retained for this instrumentation callback.
        error: &'call XllError,
        /// Native application or vendor code, when present.
        vendor_code: Option<i32>,
    },
    /// The invocation ended because cancellation was requested.
    Cancelled,
}

impl<'call> UdfCompletionOutcome<'call> {
    /// Returns the native application or vendor code when the result carries one.
    pub const fn vendor_code(self) -> Option<i32> {
        match self {
            Self::Error { vendor_code, .. } => vendor_code,
            Self::Success | Self::Cancelled => None,
        }
    }

    pub(crate) const fn trace_label(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error { kind, .. } => match kind {
                UdfErrorKind::Input => "input",
                UdfErrorKind::Domain => "domain",
                UdfErrorKind::Vendor => "vendor",
                UdfErrorKind::ExcelValue => "excelValue",
                UdfErrorKind::Panic => "panic",
                UdfErrorKind::Closing => "closing",
                UdfErrorKind::Internal => "internal",
            },
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn from_error(error: &'call XllError) -> Self {
        let (kind, vendor_code) = classify_error(error);
        Self::Error {
            kind,
            error,
            vendor_code,
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
/// Result of delivering an asynchronous calculation back to Excel.
pub enum UdfDeliveryOutcome<'call> {
    /// The invocation used a synchronous return boundary.
    NotApplicable,
    /// Excel accepted the asynchronous result.
    Delivered,
    /// Delivery failed after the Rust calculation completed.
    Failed {
        /// Error reported by the asynchronous delivery boundary.
        error: &'call XllError,
    },
    /// Delivery was unavailable or suppressed, so acceptance was not observed.
    Unobserved,
}

impl UdfDeliveryOutcome<'_> {
    pub(crate) const fn trace_label(self) -> &'static str {
        match self {
            Self::NotApplicable => "notApplicable",
            Self::Delivered => "delivered",
            Self::Failed { .. } => "failed",
            Self::Unobserved => "unobserved",
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
/// Completion, delivery, and elapsed time supplied to a layer's exit guard.
pub struct CallOutcome<'call> {
    /// Outcome of the Rust calculation itself.
    pub completion: UdfCompletionOutcome<'call>,
    /// Acceptance of an asynchronous result by Excel, when applicable.
    pub delivery: UdfDeliveryOutcome<'call>,
    /// Elapsed time measured by the framework for this invocation.
    pub duration: Duration,
}

impl<'call> CallOutcome<'call> {
    pub(crate) fn success(duration: Duration) -> Self {
        Self {
            completion: UdfCompletionOutcome::Success,
            delivery: UdfDeliveryOutcome::NotApplicable,
            duration,
        }
    }

    pub(crate) fn from_error(error: &'call XllError, duration: Duration) -> Self {
        Self {
            completion: UdfCompletionOutcome::from_error(error),
            delivery: UdfDeliveryOutcome::NotApplicable,
            duration,
        }
    }
}

/// Per-invocation instrumentation ownership returned by [`UdfLayer::enter`].
///
/// Guards are consumed once at completion. Panics in `exit` are contained;
/// destructors and completion code must still obey the bounded-work contract.
pub trait UdfLayerGuard: Send + 'static {
    /// Completes instrumentation in bounded time without waiting on an
    /// uninterruptible external operation.
    fn exit(self, outcome: &CallOutcome<'_>);
}

/// Admission or instrumentation policy shared by worksheet invocations.
///
/// Layers run in tuple order on entry and reverse tuple order on exit. A
/// failed or panicking entry rejects the invocation and completes guards from
/// earlier layers in reverse order. Layer methods may run concurrently.
///
/// Layers receive invocation metadata and completion status for admission,
/// timing, diagnostics, and metrics. They do not receive argument or return
/// values, cannot transform them, and cannot replace the UDF implementation.
/// Return an error from `enter` to reject admission before the UDF executes.
pub trait UdfLayer: Send + Sync + 'static {
    /// Owned guard retained until this invocation completes.
    type Guard: UdfLayerGuard;

    /// Enters instrumentation in bounded time.
    fn enter(&self, metadata: &CallMetadata) -> XllResult<Self::Guard>;
}

mod private {
    use super::{CallMetadata, CallOutcome, UdfLayerGuard};
    use crate::{XllError, XllResult};
    use std::time::Duration;

    pub trait LayerRejection {
        fn outcome(&self) -> CallOutcome<'_>;
    }

    pub trait UdfLayersImpl: Send + Sync + 'static {
        type Guards: UdfLayerGuard;
        const HAS_LAYERS: bool;

        fn enter_layers(&self, metadata: &CallMetadata) -> XllResult<Self::Guards> {
            self.enter_layers_with_rejection(metadata, |error| (error, Duration::ZERO))
                .map_err(|(error, _)| error)
        }

        #[doc(hidden)]
        fn enter_layers_with_rejection<R: LayerRejection>(
            &self,
            metadata: &CallMetadata,
            reject: impl FnOnce(XllError) -> R,
        ) -> Result<Self::Guards, R>;
    }
}

pub(crate) use private::LayerRejection;

impl LayerRejection for (XllError, Duration) {
    fn outcome(&self) -> CallOutcome<'_> {
        CallOutcome::from_error(&self.0, self.1)
    }
}

/// Sealed composition accepted by [`crate::Addin::Layers`].
///
/// Implemented for `()` and tuples of one to sixteen [`UdfLayer`] values.
/// Use `(layer,)` for a single layer. Implement [`UdfLayer`] for your policy
/// type and select a tuple; downstream crates cannot implement this trait
/// directly. Entry follows tuple order and exit follows reverse tuple order.
#[allow(
    private_bounds,
    reason = "Layer composition is framework-owned; users compose UdfLayer values"
)]
pub trait UdfLayers: private::UdfLayersImpl {}

impl<T> UdfLayers for T where T: private::UdfLayersImpl {}

pub(crate) fn layers_enabled<L: UdfLayers>() -> bool {
    <L as private::UdfLayersImpl>::HAS_LAYERS
}

#[allow(
    private_bounds,
    private_interfaces,
    reason = "The execution facade exposes only the sealed layer composition"
)]
pub(crate) fn enter_layers<L: UdfLayers, R: LayerRejection>(
    layers: &L,
    metadata: &CallMetadata,
    reject: impl FnOnce(XllError) -> R,
) -> Result<<L as private::UdfLayersImpl>::Guards, R> {
    <L as private::UdfLayersImpl>::enter_layers_with_rejection(layers, metadata, reject)
}

pub(crate) fn exit_layers<G: UdfLayerGuard>(guards: G, outcome: &CallOutcome<'_>) {
    safe_exit(guards, outcome);
}

pub(crate) fn safe_enter<L: UdfLayer>(layer: &L, metadata: &CallMetadata) -> XllResult<L::Guard> {
    catch_no_unwind(AssertUnwindSafe(|| layer.enter(metadata))).unwrap_or(Err(XllError::Panic))
}

pub(crate) fn safe_exit<G: UdfLayerGuard>(guard: G, outcome: &CallOutcome<'_>) {
    let _ = catch_no_unwind(AssertUnwindSafe(|| guard.exit(outcome)));
}

impl private::UdfLayersImpl for () {
    type Guards = ();
    const HAS_LAYERS: bool = false;

    fn enter_layers_with_rejection<R: LayerRejection>(
        &self,
        _metadata: &CallMetadata,
        _reject: impl FnOnce(XllError) -> R,
    ) -> Result<(), R> {
        Ok(())
    }
}

impl UdfLayerGuard for () {
    fn exit(self, _outcome: &CallOutcome<'_>) {}
}

macro_rules! impl_udf_layers {
    ($($T:ident),+ ; $($idx:tt),+ ; $($prev:ident),* ; $last:ident) => {
        impl<$($T: UdfLayer),+> private::UdfLayersImpl for ($($T,)+) {
            type Guards = ($($T::Guard,)+);
            const HAS_LAYERS: bool = true;

            #[allow(
                non_snake_case,
                reason = "Tuple guard bindings match generic type names"
            )]
            fn enter_layers_with_rejection<R: LayerRejection>(
                &self,
                metadata: &CallMetadata,
                reject: impl FnOnce(XllError) -> R,
            ) -> Result<Self::Guards, R> {
                impl_udf_layers_enter!(@step self, metadata, reject; $($T, $idx);+)
            }
        }

        impl<$($T: UdfLayerGuard),+> UdfLayerGuard for ($($T,)+) {
            #[allow(non_snake_case, reason = "Tuple unpacking matching generic type names")]
            fn exit(self, outcome: &CallOutcome<'_>) {
                let ($($T,)+) = self;
                impl_udf_layers_exit!(outcome; $($T),+);
            }
        }
    };
}

macro_rules! impl_udf_layers_enter {
    (@step $self:expr, $metadata:expr, $reject:expr; $T0:ident, 0) => {
        {
            let $T0 = safe_enter(&$self.0, $metadata).map_err($reject)?;
            Ok(($T0,))
        }
    };
    (@step $self:expr, $metadata:expr, $reject:expr; $T0:ident, 0; $($T:ident, $idx:tt);+) => {
        {
            let $T0 = match safe_enter(&$self.0, $metadata) {
                Ok(guard) => guard,
                Err(error) => return Err($reject(error)),
            };
            impl_udf_layers_enter!(@chain $self, $metadata, $reject; ($T0); $($T, $idx);+)
        }
    };
    (@chain $self:expr, $metadata:expr, $reject:expr; ($($prev:ident),+); $curr:ident, $idx:tt) => {
        {
            let $curr = match safe_enter(&$self.$idx, $metadata) {
                Ok(guard) => guard,
                Err(error) => {
                    let rejection = $reject(error);
                    let outcome = rejection.outcome();
                    impl_udf_layers_exit!(&outcome; $($prev),+);
                    return Err(rejection);
                }
            };
            Ok(($($prev,)+ $curr,))
        }
    };
    (@chain $self:expr, $metadata:expr, $reject:expr; ($($prev:ident),+); $curr:ident, $idx:tt; $($rest_T:ident, $rest_idx:tt);+) => {
        {
            let $curr = match safe_enter(&$self.$idx, $metadata) {
                Ok(guard) => guard,
                Err(error) => {
                    let rejection = $reject(error);
                    let outcome = rejection.outcome();
                    impl_udf_layers_exit!(&outcome; $($prev),+);
                    return Err(rejection);
                }
            };
            impl_udf_layers_enter!(@chain $self, $metadata, $reject; ($($prev,)+ $curr); $($rest_T, $rest_idx);+)
        }
    };
}

macro_rules! impl_udf_layers_exit {
    ($outcome:expr; $single:ident) => {
        safe_exit($single, $outcome);
    };
    ($outcome:expr; $head:ident, $($tail:ident),+) => {
        impl_udf_layers_exit!($outcome; $($tail),+);
        safe_exit($head, $outcome);
    };
}

impl_udf_layers!(T0; 0; ; T0);
impl_udf_layers!(T0, T1; 0, 1; T0; T1);
impl_udf_layers!(T0, T1, T2; 0, 1, 2; T0, T1; T2);
impl_udf_layers!(T0, T1, T2, T3; 0, 1, 2, 3; T0, T1, T2; T3);
impl_udf_layers!(T0, T1, T2, T3, T4; 0, 1, 2, 3, 4; T0, T1, T2, T3; T4);
impl_udf_layers!(T0, T1, T2, T3, T4, T5; 0, 1, 2, 3, 4, 5; T0, T1, T2, T3, T4; T5);
impl_udf_layers!(T0, T1, T2, T3, T4, T5, T6; 0, 1, 2, 3, 4, 5, 6; T0, T1, T2, T3, T4, T5; T6);
impl_udf_layers!(T0, T1, T2, T3, T4, T5, T6, T7; 0, 1, 2, 3, 4, 5, 6, 7; T0, T1, T2, T3, T4, T5, T6; T7);
impl_udf_layers!(T0, T1, T2, T3, T4, T5, T6, T7, T8; 0, 1, 2, 3, 4, 5, 6, 7, 8; T0, T1, T2, T3, T4, T5, T6, T7; T8);
impl_udf_layers!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9; 0, 1, 2, 3, 4, 5, 6, 7, 8, 9; T0, T1, T2, T3, T4, T5, T6, T7, T8; T9);
impl_udf_layers!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10; 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10; T0, T1, T2, T3, T4, T5, T6, T7, T8, T9; T10);
impl_udf_layers!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11; 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11; T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10; T11);
impl_udf_layers!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12; 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12; T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11; T12);
impl_udf_layers!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13; 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13; T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12; T13);
impl_udf_layers!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14; 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14; T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13; T14);
impl_udf_layers!(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15; 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15; T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14; T15);

pub(crate) fn classify_error(error: &XllError) -> (UdfErrorKind, Option<i32>) {
    match error {
        XllError::Input { .. } => (UdfErrorKind::Input, None),
        XllError::Domain { .. } => (UdfErrorKind::Domain, None),
        XllError::Native { code, .. } => (UdfErrorKind::Vendor, Some(*code)),
        XllError::ExcelValue(_) | XllError::Custom { .. } | XllError::Application { .. } => {
            (UdfErrorKind::ExcelValue, None)
        }
        XllError::Panic => (UdfErrorKind::Panic, None),
        XllError::Closing => (UdfErrorKind::Closing, None),
        _ => (UdfErrorKind::Internal, None),
    }
}

pub(crate) fn trace(metadata: &UdfTraceMetadata, outcome: &CallOutcome<'_>) {
    let _ = catch_no_unwind(AssertUnwindSafe(|| {
        tracing::event!(
            target: UDF_TRACE_TARGET,
            tracing::Level::INFO,
            udf = metadata.udf_id,
            excel_name = metadata.excel_name,
            call_id = metadata.call_id.get(),
            calculation_id = metadata.calculation_id.get(),
            duration_ns = outcome.duration.as_nanos().min(u64::MAX as u128) as u64,
            completion = outcome.completion.trace_label(),
            delivery = outcome.delivery.trace_label(),
            vendor_code = outcome.completion.vendor_code(),
            concurrent_calls = metadata.concurrent_calls,
            "UDF invocation completed"
        );
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::Mutex;
    use std::sync::Arc;

    struct TraceProbeSubscriber {
        panic_on_enabled: bool,
    }

    impl tracing::Subscriber for TraceProbeSubscriber {
        fn register_callsite(
            &self,
            _: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::sometimes()
        }

        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            assert!(!self.panic_on_enabled, "injected subscriber panic");
            metadata.target() == UDF_TRACE_TARGET
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, _: &tracing::Event<'_>) {}
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn trace_probe_observes_thread_scoped_dispatchers() {
        tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
            assert!(!udf_trace_enabled());
            tracing::subscriber::with_default(
                TraceProbeSubscriber {
                    panic_on_enabled: false,
                },
                || assert!(udf_trace_enabled()),
            );
            assert!(!udf_trace_enabled());
        });
    }

    #[test]
    fn trace_probe_contains_subscriber_panics() {
        tracing::subscriber::with_default(
            TraceProbeSubscriber {
                panic_on_enabled: true,
            },
            || assert!(!udf_trace_enabled()),
        );
    }

    #[test]
    fn intentional_worksheet_errors_are_classified_separately_from_internal_failures() {
        let errors = [
            XllError::ExcelValue(crate::ExcelError::NotAvailable),
            XllError::custom(crate::ExcelError::Number, "value outside application range"),
        ];
        for error in &errors {
            assert_eq!(classify_error(error), (UdfErrorKind::ExcelValue, None));
            let completion = UdfCompletionOutcome::from_error(error);
            assert_eq!(completion.trace_label(), "excelValue");
            assert_eq!(completion.vendor_code(), None);
        }
        assert_eq!(
            classify_error(&XllError::Panic),
            (UdfErrorKind::Panic, None)
        );
    }

    #[derive(Clone)]
    struct OrderedLayer {
        name: &'static str,
        order: Arc<Mutex<Vec<&'static str>>>,
        reject: bool,
    }

    struct OrderedGuard {
        name: &'static str,
        order: Arc<Mutex<Vec<&'static str>>>,
    }

    #[derive(Clone)]
    struct PanicExitLayer {
        name: &'static str,
        order: Arc<Mutex<Vec<&'static str>>>,
        panic_on_exit: bool,
    }

    struct PanicExitGuard {
        name: &'static str,
        order: Arc<Mutex<Vec<&'static str>>>,
        panic_on_exit: bool,
    }

    impl UdfLayer for OrderedLayer {
        type Guard = OrderedGuard;

        fn enter(&self, _metadata: &CallMetadata) -> XllResult<Self::Guard> {
            self.order.lock().push(self.name);
            if self.reject {
                Err(XllError::Overloaded)
            } else {
                Ok(OrderedGuard {
                    name: self.name,
                    order: Arc::clone(&self.order),
                })
            }
        }
    }

    impl UdfLayerGuard for OrderedGuard {
        fn exit(self, _outcome: &CallOutcome<'_>) {
            self.order.lock().push(match self.name {
                "enter-a" => "exit-a",
                "enter-b" => "exit-b",
                _ => "exit",
            });
        }
    }

    impl UdfLayer for PanicExitLayer {
        type Guard = PanicExitGuard;

        fn enter(&self, _metadata: &CallMetadata) -> XllResult<Self::Guard> {
            Ok(PanicExitGuard {
                name: self.name,
                order: Arc::clone(&self.order),
                panic_on_exit: self.panic_on_exit,
            })
        }
    }

    impl UdfLayerGuard for PanicExitGuard {
        fn exit(self, _outcome: &CallOutcome<'_>) {
            self.order.lock().push(self.name);
            assert!(!self.panic_on_exit, "injected layer cleanup panic");
        }
    }

    fn metadata() -> CallMetadata {
        CallMetadata {
            udf_id: "test",
            excel_name: "TEST",
            call_id: CallId::new(1),
            calculation_id: CalculationId::new(1),
            started_at: SystemTime::UNIX_EPOCH,
            concurrent_calls: 1,
        }
    }

    #[test]
    fn layers_exit_in_reverse_order() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let layers = (
            OrderedLayer {
                name: "enter-a",
                order: Arc::clone(&order),
                reject: false,
            },
            OrderedLayer {
                name: "enter-b",
                order: Arc::clone(&order),
                reject: false,
            },
        );
        let guards =
            crate::execution::enter_layers(&layers, &metadata(), |err| (err, Duration::ZERO))
                .unwrap();
        crate::execution::exit_layers(
            guards,
            &CallOutcome {
                completion: UdfCompletionOutcome::Success,
                delivery: UdfDeliveryOutcome::NotApplicable,
                duration: Duration::ZERO,
            },
        );
        assert_eq!(
            order.lock().as_slice(),
            ["enter-a", "enter-b", "exit-b", "exit-a"]
        );
    }

    #[test]
    fn rejected_entry_unwinds_previously_entered_layers() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let layers = (
            OrderedLayer {
                name: "enter-a",
                order: Arc::clone(&order),
                reject: false,
            },
            OrderedLayer {
                name: "reject",
                order: Arc::clone(&order),
                reject: true,
            },
        );
        assert!(matches!(
            crate::execution::enter_layers(&layers, &metadata(), |err| (err, Duration::ZERO)),
            Err((XllError::Overloaded, _))
        ));
        assert_eq!(order.lock().as_slice(), ["enter-a", "reject", "exit-a"]);
    }

    #[test]
    fn rejected_observation_shares_one_final_outcome_with_all_entered_layers() {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum Delivery {
            Accepted,
            Rejected,
            Unobserved,
        }

        type Events = Arc<Mutex<Vec<(&'static str, &'static str, Duration)>>>;

        struct Layer {
            name: &'static str,
            events: Events,
            fail_entry: bool,
            panic_on_exit: bool,
        }

        impl UdfLayer for Layer {
            type Guard = Self;

            fn enter(&self, _: &CallMetadata) -> XllResult<Self::Guard> {
                self.events
                    .lock()
                    .push((self.name, "enter", Duration::ZERO));
                if self.fail_entry {
                    Err(XllError::Overloaded)
                } else {
                    Ok(Self {
                        events: Arc::clone(&self.events),
                        ..*self
                    })
                }
            }
        }

        impl UdfLayerGuard for Layer {
            fn exit(self, outcome: &CallOutcome<'_>) {
                self.events.lock().push((
                    self.name,
                    outcome.delivery.trace_label(),
                    outcome.duration,
                ));
                assert!(!self.panic_on_exit, "injected layer exit panic");
            }
        }

        struct Rejection {
            cause: XllError,
            delivery: Delivery,
            delivery_error: XllError,
            duration: Duration,
        }

        impl LayerRejection for Rejection {
            fn outcome(&self) -> CallOutcome<'_> {
                CallOutcome {
                    completion: UdfCompletionOutcome::from_error(&self.cause),
                    delivery: match self.delivery {
                        Delivery::Accepted => UdfDeliveryOutcome::Delivered,
                        Delivery::Rejected => UdfDeliveryOutcome::Failed {
                            error: &self.delivery_error,
                        },
                        Delivery::Unobserved => UdfDeliveryOutcome::Unobserved,
                    },
                    duration: self.duration,
                }
            }
        }

        for delivery in [Delivery::Accepted, Delivery::Rejected, Delivery::Unobserved] {
            let events = Arc::new(Mutex::new(Vec::new()));
            let layer = |name, fail_entry, panic_on_exit| Layer {
                name,
                events: Arc::clone(&events),
                fail_entry,
                panic_on_exit,
            };
            let entry = ObservationEntry {
                timer: CallTimer::start(),
                trace_metadata: UdfTraceMetadata {
                    udf_id: "test",
                    excel_name: "TEST",
                    call_id: CallId::new(1),
                    calculation_id: CalculationId::new(1),
                    concurrent_calls: 1,
                },
                trace_enabled: false,
            };
            let layers = (
                layer("outer", false, false),
                layer("inner", false, true),
                layer("rejecting", true, false),
                layer("not entered", false, false),
            );
            let result = entry.enter(&layers, |cause, _| {
                events.lock().push(("boundary", "finalize", Duration::ZERO));
                Rejection {
                    cause,
                    delivery,
                    delivery_error: XllError::Closing,
                    duration: Duration::from_millis(3),
                }
            });
            let Err(rejection) = result else {
                panic!("the third layer must reject entry");
            };
            let finalized = rejection.finish(|_| {});
            let outcome = finalized.outcome();
            assert!(matches!(
                outcome.completion,
                UdfCompletionOutcome::Error {
                    error: XllError::Overloaded,
                    ..
                }
            ));
            let label = outcome.delivery.trace_label();
            assert_eq!(
                events.lock().as_slice(),
                [
                    ("outer", "enter", Duration::ZERO),
                    ("inner", "enter", Duration::ZERO),
                    ("rejecting", "enter", Duration::ZERO),
                    ("boundary", "finalize", Duration::ZERO),
                    ("inner", label, outcome.duration),
                    ("outer", label, outcome.duration),
                ]
            );
        }
    }

    #[test]
    fn panicking_layer_exit_does_not_skip_outer_layers() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let layers = (
            PanicExitLayer {
                name: "outer",
                order: Arc::clone(&order),
                panic_on_exit: false,
            },
            PanicExitLayer {
                name: "inner",
                order: Arc::clone(&order),
                panic_on_exit: true,
            },
        );
        let guards =
            crate::execution::enter_layers(&layers, &metadata(), |err| (err, Duration::ZERO))
                .unwrap();
        let result = catch_unwind(AssertUnwindSafe(|| {
            crate::execution::exit_layers(
                guards,
                &CallOutcome {
                    completion: UdfCompletionOutcome::Success,
                    delivery: UdfDeliveryOutcome::NotApplicable,
                    duration: Duration::ZERO,
                },
            );
        }));
        assert!(result.is_ok());
        assert_eq!(order.lock().as_slice(), ["inner", "outer"]);
    }
}
