// module: runtime
use super::open_txn::{HostAttached, OpeningTxn};
use super::transactions::open_addin_inner;
use super::{Runtime, UnloadPolicy};
use crate::addin::BuildInfo;
use crate::registration::RegistrationDescriptor;

fn invalid<A: crate::Addin>(runtime: &Runtime<A, impl UnloadPolicy<A>>, build: BuildInfo, descriptors: &[RegistrationDescriptor], transaction: OpeningTxn<'_, A, HostAttached<A>>) {
    let _ = open_addin_inner(build, descriptors, transaction, runtime); // error: E0061
}
