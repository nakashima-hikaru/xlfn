use crate::{XllError, XllResult};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CallId(u64);

impl CallId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for CallId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CalculationId(u64);

impl CalculationId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for CalculationId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug)]
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

pub(crate) struct InstrumentationPlan {
    layers: Option<Arc<SharedUdfLayers>>,
    trace_enabled: bool,
}

impl InstrumentationPlan {
    pub(crate) fn for_runtime<S>(runtime: &crate::Runtime<S>) -> Self {
        Self {
            layers: runtime.layers_if_configured(),
            trace_enabled: udf_trace_enabled(),
        }
    }

    pub(crate) const fn enabled(&self) -> bool {
        self.layers.is_some() || self.trace_enabled
    }

    pub(crate) fn layers(&self) -> Option<&SharedUdfLayers> {
        self.layers.as_deref()
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UdfResultKind {
    Success,
    InputError,
    DomainError,
    VendorError,
    Panic,
    Closing,
    InternalError,
}

#[derive(Clone, Copy, Debug)]
pub struct CallOutcome<'call> {
    pub result: UdfResultKind,
    pub error: Option<&'call XllError>,
    pub vendor_code: Option<i32>,
    pub duration: Duration,
}

pub trait UdfLayerGuard: Send + 'static {
    /// Completes instrumentation in bounded time without waiting on an
    /// uninterruptible external operation.
    fn exit(self: Box<Self>, outcome: &CallOutcome<'_>);
}

pub trait UdfLayer: Send + Sync + 'static {
    /// Enters instrumentation in bounded time.
    fn enter(&self, metadata: &CallMetadata) -> XllResult<Box<dyn UdfLayerGuard>>;
}

pub(crate) type SharedUdfLayers = Vec<Arc<dyn UdfLayer>>;

pub(crate) struct EnteredLayers {
    guards: Vec<Box<dyn UdfLayerGuard>>,
}

impl EnteredLayers {
    pub(crate) fn enter(layers: &SharedUdfLayers, metadata: &CallMetadata) -> XllResult<Self> {
        let mut entered = Self {
            guards: Vec::with_capacity(layers.len()),
        };
        for layer in layers.iter() {
            let entry = catch_unwind(AssertUnwindSafe(|| layer.enter(metadata)))
                .unwrap_or(Err(XllError::Panic));
            match entry {
                Ok(guard) => entered.guards.push(guard),
                Err(error) => {
                    let outcome = outcome_for_error(&error, Duration::ZERO);
                    exit_guards(std::mem::take(&mut entered.guards), &outcome);
                    return Err(error);
                }
            }
        }
        Ok(entered)
    }

    pub(crate) fn exit(mut self, outcome: &CallOutcome<'_>) {
        exit_guards(std::mem::take(&mut self.guards), outcome);
    }
}

impl Drop for EnteredLayers {
    fn drop(&mut self) {
        if self.guards.is_empty() {
            return;
        }
        let outcome = CallOutcome {
            result: UdfResultKind::InternalError,
            error: None,
            vendor_code: None,
            duration: Duration::ZERO,
        };
        exit_guards_no_unwind(std::mem::take(&mut self.guards), &outcome);
    }
}

fn exit_guards(mut guards: Vec<Box<dyn UdfLayerGuard>>, outcome: &CallOutcome<'_>) {
    loop {
        let Some(guard) = guards.pop() else {
            break;
        };
        // A layer is user-provided instrumentation. Its cleanup must not
        // prevent outer layers from observing the call or escape the async
        // completion worker.
        drop(catch_unwind(AssertUnwindSafe(|| guard.exit(outcome))));
    }
}

fn exit_guards_no_unwind(mut guards: Vec<Box<dyn UdfLayerGuard>>, outcome: &CallOutcome<'_>) {
    loop {
        let Some(guard) = guards.pop() else {
            break;
        };
        // This path runs while unwinding from a layer or user panic. A second
        // panic during cleanup must not turn a recoverable Excel error into an
        // abort.
        drop(catch_unwind(AssertUnwindSafe(|| guard.exit(outcome))));
    }
}

pub(crate) fn classify(error: &XllError) -> (UdfResultKind, Option<i32>) {
    match error {
        XllError::Input { .. } => (UdfResultKind::InputError, None),
        XllError::Domain { .. } => (UdfResultKind::DomainError, None),
        XllError::Native { code, .. } => (UdfResultKind::VendorError, Some(*code)),
        XllError::Panic => (UdfResultKind::Panic, None),
        XllError::Closing => (UdfResultKind::Closing, None),
        _ => (UdfResultKind::InternalError, None),
    }
}

pub(crate) fn outcome_for_error(error: &XllError, duration: Duration) -> CallOutcome<'_> {
    let (result, vendor_code) = classify(error);
    CallOutcome {
        result,
        error: Some(error),
        vendor_code,
        duration,
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
            result = ?outcome.result,
            vendor_code = outcome.vendor_code,
            concurrent_calls = metadata.concurrent_calls,
            "UDF invocation completed"
        );
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    struct OrderedLayer {
        name: &'static str,
        order: Arc<Mutex<Vec<&'static str>>>,
        reject: bool,
    }

    struct OrderedGuard {
        name: &'static str,
        order: Arc<Mutex<Vec<&'static str>>>,
    }

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
        fn enter(&self, _metadata: &CallMetadata) -> XllResult<Box<dyn UdfLayerGuard>> {
            self.order.lock().push(self.name);
            if self.reject {
                Err(XllError::Overloaded)
            } else {
                Ok(Box::new(OrderedGuard {
                    name: self.name,
                    order: Arc::clone(&self.order),
                }))
            }
        }
    }

    impl UdfLayerGuard for OrderedGuard {
        fn exit(self: Box<Self>, _outcome: &CallOutcome<'_>) {
            self.order.lock().push(match self.name {
                "enter-a" => "exit-a",
                "enter-b" => "exit-b",
                _ => "exit",
            });
        }
    }

    impl UdfLayer for PanicExitLayer {
        fn enter(&self, _metadata: &CallMetadata) -> XllResult<Box<dyn UdfLayerGuard>> {
            Ok(Box::new(PanicExitGuard {
                name: self.name,
                order: Arc::clone(&self.order),
                panic_on_exit: self.panic_on_exit,
            }))
        }
    }

    impl UdfLayerGuard for PanicExitGuard {
        fn exit(self: Box<Self>, _outcome: &CallOutcome<'_>) {
            self.order.lock().push(self.name);
            assert!(!self.panic_on_exit, "injected layer cleanup panic");
        }
    }

    fn metadata() -> CallMetadata {
        CallMetadata {
            udf_id: "test",
            excel_name: "TEST",
            call_id: 1_u64.into(),
            calculation_id: 1_u64.into(),
            started_at: SystemTime::UNIX_EPOCH,
            concurrent_calls: 1,
        }
    }

    #[test]
    fn layers_exit_in_reverse_order() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let layers: SharedUdfLayers = vec![
            Arc::new(OrderedLayer {
                name: "enter-a",
                order: Arc::clone(&order),
                reject: false,
            }),
            Arc::new(OrderedLayer {
                name: "enter-b",
                order: Arc::clone(&order),
                reject: false,
            }),
        ];
        let entered = EnteredLayers::enter(&layers, &metadata()).unwrap();
        entered.exit(&CallOutcome {
            result: UdfResultKind::Success,
            error: None,
            vendor_code: None,
            duration: Duration::ZERO,
        });
        assert_eq!(
            order.lock().as_slice(),
            ["enter-a", "enter-b", "exit-b", "exit-a"]
        );
    }

    #[test]
    fn rejected_entry_unwinds_previously_entered_layers() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let layers: SharedUdfLayers = vec![
            Arc::new(OrderedLayer {
                name: "enter-a",
                order: Arc::clone(&order),
                reject: false,
            }),
            Arc::new(OrderedLayer {
                name: "reject",
                order: Arc::clone(&order),
                reject: true,
            }),
        ];
        assert!(matches!(
            EnteredLayers::enter(&layers, &metadata()),
            Err(XllError::Overloaded)
        ));
        assert_eq!(order.lock().as_slice(), ["enter-a", "reject", "exit-a"]);
    }

    #[test]
    fn panicking_layer_exit_does_not_skip_outer_layers() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let layers: SharedUdfLayers = vec![
            Arc::new(PanicExitLayer {
                name: "outer",
                order: Arc::clone(&order),
                panic_on_exit: false,
            }),
            Arc::new(PanicExitLayer {
                name: "inner",
                order: Arc::clone(&order),
                panic_on_exit: true,
            }),
        ];
        let entered = EnteredLayers::enter(&layers, &metadata()).unwrap();
        let result = catch_unwind(AssertUnwindSafe(|| {
            entered.exit(&CallOutcome {
                result: UdfResultKind::Success,
                error: None,
                vendor_code: None,
                duration: Duration::ZERO,
            });
        }));
        assert!(result.is_ok());
        assert_eq!(order.lock().as_slice(), ["inner", "outer"]);
    }
}
