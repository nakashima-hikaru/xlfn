// module: handle
use super::object::PendingObjectBinding;
use super::publication::PublicationReservation;
fn invalid<'a>(reservation: PublicationReservation<'a>, pending: PendingObjectBinding<'a, u32>) {
    let _ = reservation.insert_object::<u32>(pending); // error: E0308
}
