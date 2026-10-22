//! Deliberately orphaned process-heap buffers for a physical DLL reload probe.
//!
//! This DLL does not register an add-in, enter a UDF, publish a result, or acquire
//! module residency. The runner must unload it after every measurement. Leaked
//! builders retain only their process-owned FP12 allocation and its quota charge;
//! no boxed Rust owner or executable callback remains after physical unload.

use xlfn::output::{
    MAX_PROCESS_NUMERIC_RETURN_BYTES, NumericArrayBuilder, numeric_return_memory_metrics,
};

/// Fixed-width FFI snapshot; keep the Python field order in sync.
#[repr(C)]
pub struct ProbeMetrics {
    abi_version: u64,
    process_reserved_bytes: u64,
    process_reserved_buffers: u64,
    process_peak_reserved_bytes: u64,
    process_published_bytes: u64,
    process_published_buffers: u64,
    process_peak_published_bytes: u64,
    process_limit_bytes: u64,
}

const _: () = assert!(size_of::<ProbeMetrics>() == 64);

/// Writes the current process ledger, returning one on success or zero on error.
///
/// # Safety
///
/// A non-null `output` must point to an aligned, writable `ProbeMetrics` object.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn fp12_probe_read_metrics(output: *mut ProbeMetrics) -> i32 {
    if output.is_null() {
        return 0;
    }
    let Ok(metrics) = numeric_return_memory_metrics() else {
        return 0;
    };
    let snapshot = ProbeMetrics {
        abi_version: 1,
        process_reserved_bytes: metrics.process_reserved_bytes as u64,
        process_reserved_buffers: metrics.process_reserved_buffers as u64,
        process_peak_reserved_bytes: metrics.process_peak_reserved_bytes as u64,
        process_published_bytes: metrics.process_published_bytes as u64,
        process_published_buffers: metrics.process_published_buffers as u64,
        process_peak_published_bytes: metrics.process_peak_published_bytes as u64,
        process_limit_bytes: MAX_PROCESS_NUMERIC_RETURN_BYTES as u64,
    };
    // SAFETY: the caller provides the aligned writable output object documented
    // above, and a primitive snapshot has no destructor or module references.
    unsafe { output.write(snapshot) };
    1
}

/// Reserves and intentionally orphans one FP12 allocation.
///
/// Returns zero when dimensions, allocation, or the aggregate quota reject the
/// request. No element beyond the constructor's initialized header is read.
#[unsafe(no_mangle)]
pub extern "system" fn fp12_probe_retain_allocation(cells: u64) -> i32 {
    let Ok(cells) = usize::try_from(cells) else {
        return 0;
    };
    let Ok(builder) = NumericArrayBuilder::new(cells, 1) else {
        return 0;
    };
    std::mem::forget(builder);
    1
}
