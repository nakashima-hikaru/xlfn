//! Optional layer/body instrumentation. Never use this build for timings.
use std::sync::atomic::{AtomicU64, Ordering};
use xlfn::{
    execution::{
        CallMetadata, CallOutcome, UdfCompletionOutcome, UdfErrorKind, UdfLayer, UdfLayerGuard,
    },
    prelude::*,
};

static ENTERED: AtomicU64 = AtomicU64::new(0);
static BODY: AtomicU64 = AtomicU64::new(0);
static INPUT_ERRORS: AtomicU64 = AtomicU64::new(0);
static OTHER_ERRORS: AtomicU64 = AtomicU64::new(0);

pub struct SemanticLayer;
pub struct SemanticGuard(bool);

impl UdfLayer for SemanticLayer {
    type Guard = SemanticGuard;
    fn enter(&self, metadata: &CallMetadata) -> XllResult<Self::Guard> {
        let observed = metadata.excel_name.starts_with("BENCH.ABI.SEM.")
            && (metadata.excel_name.ends_with(".Q") || metadata.excel_name.ends_with(".NATIVE"));
        if observed {
            ENTERED.fetch_add(1, Ordering::Relaxed);
        }
        Ok(SemanticGuard(observed))
    }
}

impl UdfLayerGuard for SemanticGuard {
    fn exit(self, outcome: &CallOutcome<'_>) {
        if self.0 {
            match outcome.completion {
                UdfCompletionOutcome::Error {
                    kind: UdfErrorKind::Input,
                    ..
                } => {
                    INPUT_ERRORS.fetch_add(1, Ordering::Relaxed);
                }
                UdfCompletionOutcome::Success => {}
                _ => {
                    OTHER_ERRORS.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

#[excel_function(name = "BENCH.ABI.SEM.COUNT", thread_safe)]
pub fn count(index: i32) -> f64 {
    let counter = match index {
        0 => &ENTERED,
        1 => &BODY,
        2 => &INPUT_ERRORS,
        _ => &OTHER_ERRORS,
    };
    counter.load(Ordering::Relaxed) as f64
}

#[excel_function(name = "BENCH.ABI.SEM.ENABLED", thread_safe)]
pub fn enabled() -> f64 {
    1.0
}

#[excel_function(name = "BENCH.ABI.SEM.F64.Q", thread_safe)]
pub fn f64_q(#[excel_arg(strict)] value: f64) -> f64 {
    BODY.fetch_add(1, Ordering::Relaxed);
    value
}

#[excel_function(name = "BENCH.ABI.SEM.F64.NATIVE", thread_safe)]
pub fn f64_native(value: f64) -> f64 {
    BODY.fetch_add(1, Ordering::Relaxed);
    value
}

#[excel_function(name = "BENCH.ABI.SEM.BOOL.Q", thread_safe)]
pub fn bool_q(#[excel_arg(strict)] value: bool) -> f64 {
    BODY.fetch_add(1, Ordering::Relaxed);
    if value { 1.0 } else { 0.0 }
}

#[excel_function(name = "BENCH.ABI.SEM.BOOL.NATIVE", thread_safe)]
pub fn bool_native(value: bool) -> f64 {
    BODY.fetch_add(1, Ordering::Relaxed);
    if value { 1.0 } else { 0.0 }
}

#[excel_function(name = "BENCH.ABI.SEM.I32.Q", thread_safe)]
pub fn i32_q(#[excel_arg(strict)] value: i32) -> f64 {
    BODY.fetch_add(1, Ordering::Relaxed);
    f64::from(value)
}

#[excel_function(name = "BENCH.ABI.SEM.I32.NATIVE", thread_safe)]
pub fn i32_native(value: i32) -> f64 {
    BODY.fetch_add(1, Ordering::Relaxed);
    f64::from(value)
}

#[excel_function(name = "BENCH.ABI.SEM.STR.Q", thread_safe)]
pub fn str_q(#[excel_arg(strict)] value: String) -> f64 {
    BODY.fetch_add(1, Ordering::Relaxed);
    value.chars().count() as f64
}

#[excel_function(name = "BENCH.ABI.SEM.STR.NATIVE", thread_safe)]
pub fn str_native(value: String) -> f64 {
    BODY.fetch_add(1, Ordering::Relaxed);
    value.chars().count() as f64
}
