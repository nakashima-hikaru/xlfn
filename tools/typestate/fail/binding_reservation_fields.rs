// module: handle
use super::binding::{BindingReservation, BindingTable};
use super::token::HandleId;

fn invalid<'table>(
    mut reservation: BindingReservation<'table>,
    other: &'table BindingTable,
    id: HandleId,
) {
    reservation.table = other; // error: E0609
    let _ = reservation.slot; // error: E0616
    reservation.index = 0; // error: E0609
    reservation.id = id; // error: E0615
    let _ = &reservation.origin; // error: E0609
    reservation.active = false; // error: E0609
}
