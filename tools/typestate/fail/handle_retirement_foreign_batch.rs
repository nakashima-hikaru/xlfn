// module: handle/domain
use super::retirement::{RetirementAccounting, UnlinkedBindingOwner};
use smallvec::SmallVec;
use xlfn_kernel::drain_gate::DEFAULT_STRIPE_COUNT;
use xlfn_kernel::rotating_read_domain::DisposableBatch;

fn invalid(
    accounting: &RetirementAccounting,
    batch: DisposableBatch<'_, DEFAULT_STRIPE_COUNT, SmallVec<[UnlinkedBindingOwner; 4]>>,
) {
    let _ = accounting.bind_batch(batch); // error: E0599
}
