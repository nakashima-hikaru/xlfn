// module: handle
use super::object::PendingObjectBinding;
use super::publication::PublicationReservation;
fn invalid<'a, T: super::ExcelHandleObject>(reservation: PublicationReservation<'a>, pending: PendingObjectBinding<'a, T>) {
    let _ = reservation.insert_object::<T>(pending); // error: E0308
}
