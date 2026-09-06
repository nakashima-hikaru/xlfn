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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CaughtPanic;

/// Consumes a caught panic without invoking an arbitrary payload destructor.
/// This also accepts a worker's `JoinHandle::join` result.
pub(crate) fn contain_panic<T>(result: std::thread::Result<T>) -> Result<T, CaughtPanic> {
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
pub(crate) fn catch_no_unwind<T>(
    operation: impl FnOnce() -> T + UnwindSafe,
) -> Result<T, CaughtPanic> {
    contain_panic(std::panic::catch_unwind(operation))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) struct PanickingPayload(pub(crate) Arc<AtomicUsize>);

    impl Drop for PanickingPayload {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
            panic!("injected panic payload destructor");
        }
    }

    #[test]
    fn success_returns_the_original_value() {
        assert_eq!(catch_no_unwind(|| 42), Ok(42));
        assert_eq!(contain_panic(Ok("joined")), Ok("joined"));
    }

    #[test]
    fn caught_payload_is_retained_without_running_its_destructor() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let payload = PanickingPayload(Arc::clone(&dropped));
        let result = catch_no_unwind(|| std::panic::panic_any(payload));
        assert_eq!(result, Err(CaughtPanic));
        assert_eq!(dropped.load(Ordering::Acquire), 0);
    }

    #[test]
    fn joined_payload_is_retained_without_running_its_destructor() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let payload = PanickingPayload(Arc::clone(&dropped));
        let worker = std::thread::spawn(|| std::panic::panic_any(payload));
        assert_eq!(contain_panic(worker.join()), Err(CaughtPanic));
        assert_eq!(dropped.load(Ordering::Acquire), 0);
    }
}
