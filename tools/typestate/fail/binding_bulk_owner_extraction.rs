// module: handle
use super::binding::BindingTable;

fn invalid(table: &BindingTable) {
    let (_live, batch) = table.retire_all();
    let _ = batch.into_inner(); // error: E0599
}
