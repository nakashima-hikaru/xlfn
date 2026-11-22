//! Bridges the FP12 allocation epoch to process-owned host accounting.
use crate::XllResult;
use std::ptr::NonNull;
pub use xlfn_host::process_quota::MAX_PROCESS_NUMERIC_RETURN_BYTES;
pub(super) use xlfn_host::process_quota::{PublishedCharge, Reservation};

/// Recovers the accounting from the actual detached allocation and releases it
/// only after that allocation was freed. No caller supplies a ledger or count.
///
/// # Safety
/// `pointer` is a uniquely detached, published block from an epoch which Excel
/// no longer reads. This function is called at most once unless free failed.
#[inline]
pub(super) unsafe fn reclaim_detached_block(
    pointer: NonNull<super::NumericBlock>,
) -> Option<usize> {
    // SAFETY: the epoch withdrawal contract grants exclusive ownership of this
    // initialized published header. The charge contains only plain data.
    let retained = unsafe { &(*pointer.as_ptr()).charge };
    // SAFETY: publication initialized this plain charge before exposing the
    // block. Its borrow ends here, before the allocation can be freed below.
    let charge = unsafe { retained.assume_init_read() };
    // SAFETY: this is the exclusively owned block which contained the
    // charge. Accounting supplies the exact original allocation size.
    let release = |bytes| unsafe { super::release_block(pointer, bytes) };
    // SAFETY: the charge was installed on this block before publication and
    // the releaser reports whether the free physically succeeded.
    unsafe { charge.reclaim(release) }
}

pub(super) fn read_metrics(metrics: &mut super::NumericReturnMemoryMetrics) -> XllResult<()> {
    let process = xlfn_host::process_quota::read_metrics()?;
    metrics.process_reserved_bytes = process.reserved_bytes;
    metrics.process_reserved_buffers = process.reserved_buffers;
    metrics.process_peak_reserved_bytes = process.peak_reserved_bytes;
    metrics.process_published_bytes = process.published_bytes;
    metrics.process_published_buffers = process.published_buffers;
    metrics.process_peak_published_bytes = process.peak_published_bytes;
    Ok(())
}
