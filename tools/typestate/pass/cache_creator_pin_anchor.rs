// module: cache
use super::{CreatorPinGuard, FlightAnchor, ResidentPin};

fn acquire(creator: &CreatorPinGuard<'_, u32>) {
    let _resident = ResidentPin::acquire(creator);
    let _flight = FlightAnchor::acquire(creator);
}
