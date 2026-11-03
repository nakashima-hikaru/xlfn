// module: runtime/shutdown/pipeline
use super::{ServicesSealed, TeardownTxn};
use crate::runtime::capabilities::InstalledLifecycle;
use crate::runtime::shutdown::FinalRemoval;
fn invalid<'a, A: crate::Addin>(txn: TeardownTxn<'a, A, FinalRemoval, ServicesSealed<'a, A>>, access: &crate::runtime::AddinLifecycleAccess<'_, A>, binding: InstalledLifecycle<'a, A>, report: &mut crate::shutdown::CloseReport) {
    let _ = txn.cleanup_addin(access, report, Some(binding)); // error: E0061
}
