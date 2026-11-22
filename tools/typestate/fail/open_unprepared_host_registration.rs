// module: runtime
use super::open_txn::{GenerationStaged, OpeningTxn};
use super::AddinLifecycleAccess;
use crate::registration::{HostRegistrar, preflight::ValidatedRegistrationSet};

fn register_without_services<A: crate::Addin>(
    transaction: OpeningTxn<'_, A, GenerationStaged<A>>,
    registrar: HostRegistrar,
    registrations: &ValidatedRegistrationSet,
) {
    let _ = transaction.register_prepared_host(registrar, registrations); // error: E0599
}

fn register_events_without_services<A: crate::Addin>(
    mut transaction: OpeningTxn<'_, A, GenerationStaged<A>>,
) {
    let _ = transaction.ensure_calculation_events(false); // error: E0599
}

fn install_without_services<A: crate::Addin>(
    transaction: OpeningTxn<'_, A, GenerationStaged<A>>,
    access: &AddinLifecycleAccess<'_, A>,
) {
    let _ = transaction.install_lifecycle(access); // error: E0599
}

fn prepare_twice<A: crate::Addin>(
    transaction: OpeningTxn<'_, A, GenerationStaged<A>>,
) {
    let _ = transaction.prepare_services();
    let _ = transaction.prepare_services(); // error: E0382
}
