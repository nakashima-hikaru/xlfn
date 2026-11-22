//! Shared panic policy for every boundary that consumes a panic.
//!
//! A caught panic's payload is arbitrary user data. Dropping it can panic
//! again, including during cleanup of another panic, so ordinary `catch_unwind`
//! followed by discarding its result is not a no-unwind boundary. These helpers
//! destroy only the standard `&'static str` and `String` payloads. Arbitrary
//! payloads are deliberately retained without running their destructors. This
//! leaks custom payloads and their resources only on that exceptional path,
//! in exchange for preserving ABI, notification, and teardown progress.
//!
//! Use ordinary `catch_unwind` only at audited boundaries that resume the
//! original payload. All consuming catches and worker joins use these helpers.
//! The exact exceptions are checked by `just panic-boundaries`. This policy does
//! not contain aborting panics, panicking panic hooks, or double panics raised
//! before unwinding reaches the boundary.

use std::mem::ManuallyDrop;
use std::panic::UnwindSafe;

/// Opaque marker that a panic was caught and its payload contained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaughtPanic;

/// Consumes a caught panic without invoking an arbitrary payload destructor.
/// This also accepts a worker's `JoinHandle::join` result.
pub fn contain_panic<T>(result: std::thread::Result<T>) -> Result<T, CaughtPanic> {
    result.map_err(|payload| {
        if payload.is::<&'static str>() || payload.is::<String>() {
            // Exact standard-library types have no user-defined destructor.
            drop(payload);
        } else {
            // Intentionally never destroyed: even trying to catch payload Drop
            // can abort if it unwinds through another panicking destructor.
            let _retained = ManuallyDrop::new(payload);
        }
        CaughtPanic
    })
}

/// Runs a panic-consuming boundary. Callers explicitly establish unwind
/// safety, using `AssertUnwindSafe` only for their audited cleanup operation.
pub fn catch_no_unwind<T>(operation: impl FnOnce() -> T + UnwindSafe) -> Result<T, CaughtPanic> {
    contain_panic(std::panic::catch_unwind(operation))
}

/// Retains a bounded standard panic message without dropping arbitrary payloads.
pub fn catch_udf_no_unwind<T>(operation: impl FnOnce() -> T + UnwindSafe) -> Result<T, String> {
    match std::panic::catch_unwind(operation) {
        Ok(value) => Ok(value),
        Err(payload) => {
            let text = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&'static str>().copied())
                .unwrap_or("UDF panicked with a non-string payload");
            let mut end = text.len().min(16 * 1024);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let mut message = String::new();
            if message.try_reserve_exact(end).is_ok() {
                message.push_str(&text[..end]);
            }
            // Apply the same arbitrary-payload destructor policy as every other boundary.
            let _ = contain_panic::<()>(Err(payload));
            Err(message)
        }
    }
}
