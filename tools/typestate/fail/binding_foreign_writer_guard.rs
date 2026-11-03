// module: handle
use super::binding::{BindingTable, RegistryState};
use crate::sync::RwLockWriteGuard;

fn invalid(table: &BindingTable, mut guard: RwLockWriteGuard<'_, RegistryState>) {
    let _ = table.publication_writer(&mut guard); // error: E0599
}
