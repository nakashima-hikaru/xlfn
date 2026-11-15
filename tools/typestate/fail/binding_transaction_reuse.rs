// module: handle
use super::binding::{BindingRemoval, BindingReservation};
use super::object::ObjectBinding;

fn publish_twice(reservation: BindingReservation<'_>, first: ObjectBinding, second: ObjectBinding) {
    let _ = reservation.publish(first);
    let _ = reservation.publish(second); // error: E0382
}

fn remove_twice(removal: BindingRemoval<'_>) {
    let _ = removal.commit();
    let _ = removal.commit(); // error: E0382
}
