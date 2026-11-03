// module: handle
use super::binding::BindingTable;

fn invalid(table: &BindingTable) {
    let (_live, batch) = table.retire_all();
    let _records = &batch.records; // error: E0616
}
