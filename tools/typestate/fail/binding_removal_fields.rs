// module: handle
use super::binding::{BindingRemoval, BindingTable};
use super::token::HandleId;

fn invalid<'table>(
    mut removal: BindingRemoval<'table>,
    other: &'table BindingTable,
    id: HandleId,
) {
    removal.table = other; // error: E0616
    let _ = removal.state.take(); // error: E0616
    removal.id = id; // error: E0616
    let _ = &removal.record; // error: E0616
    removal.active = false; // error: E0616
}
