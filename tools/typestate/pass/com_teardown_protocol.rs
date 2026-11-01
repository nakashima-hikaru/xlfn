// module: excel_rtd/windows/server
use super::{OwnedServerReference, RtdSubscription};

unsafe fn teardown<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    reference: OwnedServerReference,
) {
    // SAFETY: the caller selects the concrete type used to acquire reference.
    let Ok(access) = (unsafe { reference.drain_access::<E, C>() }) else { return };
    let _ = access.teardown_resources();
    let Ok(joined) = access.join_worker() else { return };
    let _ = joined.worker_panicked();
    let _ = joined.teardown_and_remove_active(false);
}
