//! Identical production reporting harness for isolated baseline/candidate runs.

use super::{AsyncDiagnosticSink, DiagnosticEvent, DiagnosticSink, report_no_unwind, router};
use crate::XllError;
use crate::sync::{Condvar, Mutex};
use std::fmt::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Production diagnostic reporting workload to exercise.
pub enum DiagnosticBenchCase {
    /// Reporting with diagnostic delivery disabled.
    Disabled,
    /// Worker delivery of a small error.
    SmallError,
    /// Reporting against a saturated delivery queue.
    FullQueue,
    /// A burst of large errors against bounded delivery.
    LargeBurst,
    /// Tracing formatting of a small error.
    TraceSmall,
    /// Tracing formatting of a large error.
    TraceLarge,
}

impl DiagnosticBenchCase {
    /// Returns the stable benchmark case name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::SmallError => "small_error",
            Self::FullQueue => "full_queue",
            Self::LargeBurst => "large_burst",
            Self::TraceSmall => "trace_small",
            Self::TraceLarge => "trace_large",
        }
    }

    /// Returns whether this workload uses burst delivery.
    pub const fn is_burst(self) -> bool {
        matches!(self, Self::LargeBurst)
    }
}

struct DeliveryState {
    reports: usize,
    release: bool,
}
struct DeliveryGate {
    state: Mutex<DeliveryState>,
    changed: Condvar,
}

impl DeliveryGate {
    fn wait_reports(&self, target: usize) {
        let mut state = self.state.lock();
        while state.reports < target {
            self.changed.wait(&mut state);
        }
    }
}

struct BenchSink {
    gate: Arc<DeliveryGate>,
    blocked: bool,
}
impl DiagnosticSink for BenchSink {
    fn report(&self, _: &DiagnosticEvent<'_>) {
        let mut state = self.gate.state.lock();
        state.reports += 1;
        self.gate.changed.notify_all();
        while self.blocked && !state.release {
            self.gate.changed.wait(&mut state);
        }
    }
}

struct FormatSubscriber {
    bytes: Arc<AtomicUsize>,
}
struct FormatCounter<'a>(&'a AtomicUsize);
impl Write for FormatCounter<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0.fetch_add(text.len(), Ordering::Relaxed);
        Ok(())
    }
}
impl tracing::field::Visit for FormatCounter<'_> {
    fn record_debug(&mut self, _: &tracing::field::Field, value: &dyn fmt::Debug) {
        write!(self, "{value:?}").unwrap();
    }
}
impl tracing::Subscriber for FormatSubscriber {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        event.record(&mut FormatCounter(&self.bytes));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// One fixture owns the global diagnostic router until dropped. Benchmark drivers
/// use fixtures sequentially. Setup confirms the real worker entered delivery;
/// worker startup and dispatch construction are outside measurement.
pub struct DiagnosticBenchmark {
    case: DiagnosticBenchCase,
    error: XllError,
    dispatch: tracing::Dispatch,
    formatted: Arc<AtomicUsize>,
    gate: Option<Arc<DeliveryGate>>,
}

#[derive(Debug)]
/// Observed reporting attempts, queue drops, and tracing output.
pub struct DiagnosticProbeResult {
    /// Number of attempted reports.
    pub attempts: usize,
    /// Number of events dropped by bounded delivery.
    pub dropped: u64,
    /// Bytes formatted by the tracing subscriber.
    pub formatted_bytes: usize,
}

impl DiagnosticBenchmark {
    /// Installs the diagnostic workload and waits for worker readiness.
    pub fn new(case: DiagnosticBenchCase) -> Self {
        router()
            .reset()
            .expect("benchmark starts from a closed router");
        let formatted = Arc::new(AtomicUsize::new(0));
        let dispatch = if matches!(
            case,
            DiagnosticBenchCase::TraceSmall | DiagnosticBenchCase::TraceLarge
        ) {
            tracing::Dispatch::new(FormatSubscriber {
                bytes: Arc::clone(&formatted),
            })
        } else {
            tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default())
        };
        let large = matches!(
            case,
            DiagnosticBenchCase::FullQueue
                | DiagnosticBenchCase::LargeBurst
                | DiagnosticBenchCase::TraceLarge
        );
        let error = XllError::Native {
            code: 7,
            message: "x".repeat(if large { 1024 * 1024 } else { 128 }),
        };
        let gate = if matches!(
            case,
            DiagnosticBenchCase::SmallError
                | DiagnosticBenchCase::FullQueue
                | DiagnosticBenchCase::LargeBurst
        ) {
            let gate = Arc::new(DeliveryGate {
                state: Mutex::new(DeliveryState {
                    reports: 0,
                    release: false,
                }),
                changed: Condvar::new(),
            });
            let blocked = case != DiagnosticBenchCase::SmallError;
            router()
                .replace_with(
                    || {
                        AsyncDiagnosticSink::new(BenchSink {
                            gate: Arc::clone(&gate),
                            blocked,
                        })
                        .map(Box::new)
                    },
                    |_| Ok(()),
                )
                .expect("benchmark sink publication");
            // Prime with one large event only for the byte-quota case. Its
            // processing credit must remain included in the candidate bound.
            let seed = if case == DiagnosticBenchCase::LargeBurst {
                &error
            } else {
                &XllError::Panic
            };
            tracing::dispatcher::with_default(&dispatch, || {
                report_no_unwind("diagnostic_benchmark", seed);
            });
            gate.wait_reports(1);
            if case == DiagnosticBenchCase::FullQueue {
                tracing::dispatcher::with_default(&dispatch, || {
                    for _ in 0..super::DIAGNOSTIC_QUEUE_CAPACITY {
                        report_no_unwind("diagnostic_benchmark", &XllError::Panic);
                    }
                });
            }
            Some(gate)
        } else {
            None
        };
        let fixture = Self {
            case,
            error,
            dispatch,
            formatted,
            gate,
        };
        if case != DiagnosticBenchCase::LargeBurst {
            fixture.run(1);
        }
        fixture
    }

    /// Includes reporting, cloning and (for small errors) completed deliveries.
    /// Blocked/full queues and opt-in formatting include only caller reporting.
    pub fn run(&self, attempts: usize) -> DiagnosticProbeResult {
        let before = super::diagnostic_stats().dropped_events;
        let formatted = self.formatted.load(Ordering::Relaxed);
        let target = self
            .gate
            .as_ref()
            .map(|gate| gate.state.lock().reports + attempts);
        tracing::dispatcher::with_default(&self.dispatch, || {
            for _ in 0..attempts {
                std::hint::black_box(report_no_unwind("diagnostic_benchmark", &self.error));
            }
        });
        if self.case == DiagnosticBenchCase::SmallError {
            self.gate.as_ref().unwrap().wait_reports(target.unwrap());
        }
        DiagnosticProbeResult {
            attempts,
            dropped: super::diagnostic_stats().dropped_events - before,
            formatted_bytes: self.formatted.load(Ordering::Relaxed) - formatted,
        }
    }
}

impl Drop for DiagnosticBenchmark {
    fn drop(&mut self) {
        if let Some(gate) = &self.gate {
            gate.state.lock().release = true;
            gate.changed.notify_all();
        }
        router().close_terminal().expect("benchmark flush and join");
    }
}
