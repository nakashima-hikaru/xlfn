// module: handle
use super::binding::{BindingReservation, BindingSlot, BindingTable};
use super::object::ObjectBinding;
use crate::XllResult;

fn reserve(table: &BindingTable) -> XllResult<()> {
    let reservation = table.reserve()?;
    let _id = reservation.id();
    drop(reservation);
    Ok(())
}

fn publish(reservation: BindingReservation<'_>, object: ObjectBinding) {
    let _ = reservation.publish(object);
}

fn remove(table: &BindingTable, id: super::token::HandleId) -> XllResult<()> {
    let removal = table.begin_removal(id)?;
    let _reusable = removal.commit();
    Ok(())
}

fn retire(table: &BindingTable) {
    let (_live, drained) = table.retire_all();
    drop(drained);
}

fn observe_slot(slot: &BindingSlot) {
    if let Some(record) = &slot.record {
        let _shared = record.as_ref();
    }
}
