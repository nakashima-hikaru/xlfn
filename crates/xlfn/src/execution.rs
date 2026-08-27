use crate::{XllError, XllResult};
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CallId(u64);

impl CallId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CalculationId(u64);

impl CalculationId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct CallMetadata {
    pub udf_id: &'static str,
    pub excel_name: &'static str,
    pub call_id: CallId,
    pub calculation_id: CalculationId,
    pub started_at: SystemTime,
    pub concurrent_calls: usize,
}

pub(crate) const UDF_TRACE_TARGET: &str = "xlfn::udf";

pub(crate) fn udf_trace_enabled() -> bool {
    // The common no-global-subscriber path does not need an unwind guard.
    // `enabled!` still observes a thread-scoped dispatcher, so this shortcut
    // does not change scoped instrumentation semantics.
    if !tracing::dispatcher::has_been_set() {
        return tracing::enabled!(target: UDF_TRACE_TARGET, tracing::Level::INFO);
    }
    catch_unwind(AssertUnwindSafe(
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

    pub(crate) fn has_layers(&self) -> bool {
        layers_enabled::<A::Layers>()
    }

    pub(crate) const fn trace_enabled(&self) -> bool {
        self.trace_enabled
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum UdfErrorKind {
    Input,
    Domain,
    Vendor,
    Panic,
    Closing,
    Internal,
}

#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum UdfCompletionOutcome<'call> {
    Success,
    Error {
        kind: UdfErrorKind,
        error: &'call XllError,
        vendor_code: Option<i32>,
    },
    Cancelled,
}

impl<'call> UdfCompletionOutcome<'call> {
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
pub enum UdfDeliveryOutcome<'call> {
    NotApplicable,
    Delivered,
    Failed { error: &'call XllError },
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
pub struct CallOutcome<'call> {
    pub completion: UdfCompletionOutcome<'call>,
    pub delivery: UdfDeliveryOutcome<'call>,
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

pub trait UdfLayerGuard: Send + 'static {
    /// Completes instrumentation in bounded time without waiting on an
    /// uninterruptible external operation.
    fn exit(self, outcome: &CallOutcome<'_>);
}

pub trait UdfLayer: Send + Sync + 'static {
    type Guard: UdfLayerGuard;

    /// Enters instrumentation in bounded time.
    fn enter(&self, metadata: &CallMetadata) -> XllResult<Self::Guard>;
}

mod private {
    use super::{CallMetadata, UdfLayerGuard};
    use crate::XllResult;

    pub trait UdfLayersImpl: Send + Sync + 'static {
        type Guards: UdfLayerGuard;
        const HAS_LAYERS: bool;

        fn enter_layers(&self, metadata: &CallMetadata) -> XllResult<Self::Guards>;
    }
}

#[doc(hidden)]
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
pub(crate) fn enter_layers<L: UdfLayers>(
    layers: &L,
    metadata: &CallMetadata,
) -> XllResult<<L as private::UdfLayersImpl>::Guards> {
    <L as private::UdfLayersImpl>::enter_layers(layers, metadata)
}

pub(crate) fn exit_layers<G: UdfLayerGuard>(guards: G, outcome: &CallOutcome<'_>) {
    safe_exit(guards, outcome);
}

pub(crate) fn safe_enter<L: UdfLayer>(layer: &L, metadata: &CallMetadata) -> XllResult<L::Guard> {
    catch_unwind(AssertUnwindSafe(|| layer.enter(metadata))).unwrap_or(Err(XllError::Panic))
}

pub(crate) fn safe_exit<G: UdfLayerGuard>(guard: G, outcome: &CallOutcome<'_>) {
    drop(catch_unwind(AssertUnwindSafe(|| guard.exit(outcome))));
}

impl private::UdfLayersImpl for () {
    type Guards = ();
    const HAS_LAYERS: bool = false;

    fn enter_layers(&self, _metadata: &CallMetadata) -> XllResult<()> {
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
                unused_variables,
                reason = "Variables used for unwinding earlier guards"
            )]
            fn enter_layers(&self, metadata: &CallMetadata) -> XllResult<Self::Guards> {
                impl_udf_layers_enter!(@step self, metadata; $($T, $idx);+)
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
    (@step $self:expr, $metadata:expr; $T0:ident, 0) => {
        {
            let $T0 = safe_enter(&$self.0, $metadata)?;
            Ok(($T0,))
        }
    };
    (@step $self:expr, $metadata:expr; $T0:ident, 0; $($T:ident, $idx:tt);+) => {
        {
            let $T0 = safe_enter(&$self.0, $metadata)?;
            impl_udf_layers_enter!(@chain $self, $metadata; ($T0); $($T, $idx);+)
        }
    };
    (@chain $self:expr, $metadata:expr; ($($prev:ident),+); $curr:ident, $idx:tt) => {
        {
            let $curr = match safe_enter(&$self.$idx, $metadata) {
                Ok(guard) => guard,
                Err(error) => {
                    let outcome = CallOutcome::from_error(&error, Duration::ZERO);
                    impl_udf_layers_exit!(&outcome; $($prev),+);
                    return Err(error);
                }
            };
            Ok(($($prev,)+ $curr,))
        }
    };
    (@chain $self:expr, $metadata:expr; ($($prev:ident),+); $curr:ident, $idx:tt; $($rest_T:ident, $rest_idx:tt);+) => {
        {
            let $curr = match safe_enter(&$self.$idx, $metadata) {
                Ok(guard) => guard,
                Err(error) => {
                    let outcome = CallOutcome::from_error(&error, Duration::ZERO);
                    impl_udf_layers_exit!(&outcome; $($prev),+);
                    return Err(error);
                }
            };
            impl_udf_layers_enter!(@chain $self, $metadata; ($($prev,)+ $curr); $($rest_T, $rest_idx);+)
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
        XllError::Panic => (UdfErrorKind::Panic, None),
        XllError::Closing => (UdfErrorKind::Closing, None),
        _ => (UdfErrorKind::Internal, None),
    }
}

pub(crate) fn trace(metadata: &UdfTraceMetadata, outcome: &CallOutcome<'_>) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
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
    use parking_lot::Mutex;
    use std::sync::Arc;

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
        let guards = crate::execution::enter_layers(&layers, &metadata()).unwrap();
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
            crate::execution::enter_layers(&layers, &metadata()),
            Err(XllError::Overloaded)
        ));
        assert_eq!(order.lock().as_slice(), ["enter-a", "reject", "exit-a"]);
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
        let guards = crate::execution::enter_layers(&layers, &metadata()).unwrap();
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
