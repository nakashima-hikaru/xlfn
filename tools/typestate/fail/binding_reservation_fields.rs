// module: handle
use super::binding::{BindingReservation, BindingTable};
use super::token::HandleId;

fn invalid<'table>(
    mut reservation: BindingReservation<'table>,
    other: &'table BindingTable,
    id: HandleId,
) {
    reservation.table = other; // error: E0609
    let _ = reservation.state.take(); // error: E0616
    reservation.index = 0; // error: E0616
    reservation.id = id; // error: E0616
    let _ = &reservation.origin; // error: E0616
    reservation.active = false; // error: E0616
}
