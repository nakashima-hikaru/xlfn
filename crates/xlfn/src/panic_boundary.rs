//! Compatibility imports for the engine's shared panic-containment policy.

#[cfg(any(test, feature = "rtd", feature = "handles"))]
pub(crate) use xlfn_engine::panic_boundary::CaughtPanic;
pub(crate) use xlfn_engine::panic_boundary::{catch_no_unwind, catch_udf_no_unwind, contain_panic};

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
    #[test]
    fn udf_panic_text_is_bounded_and_arbitrary_payload_is_not_destroyed() {
        let message = catch_udf_no_unwind(|| panic!("pricing failure")).unwrap_err();
        assert_eq!(message, "pricing failure");
        let long = "日".repeat(10_000);
        let message = catch_udf_no_unwind(|| std::panic::panic_any(long)).unwrap_err();
        assert!(message.len() <= 16 * 1024);
        assert!(message.chars().all(|character| character == '日'));
        let dropped = Arc::new(AtomicUsize::new(0));
        let payload = PanickingPayload(Arc::clone(&dropped));
        let message = catch_udf_no_unwind(|| std::panic::panic_any(payload)).unwrap_err();
        assert!(message.contains("non-string payload"));
        assert_eq!(dropped.load(Ordering::Acquire), 0);
    }
}
