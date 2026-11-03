// module: runtime
use super::open_txn::{HostAttached, OpeningTxn};
use super::transactions::open_addin_inner;
use crate::addin::BuildInfo;
use crate::registration::RegistrationDescriptor;

fn initialize<A: crate::Addin>(build: BuildInfo, descriptors: &[RegistrationDescriptor], transaction: OpeningTxn<'_, A, HostAttached<A>>) {
    let _ = open_addin_inner(build, descriptors, transaction);
}
