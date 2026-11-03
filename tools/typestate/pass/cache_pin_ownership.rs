// module: cache
fn ordered<V>(pin: super::pins::ResidentPin<V>, anchor: super::pins::FlightAnchor<V>) {
    let observed = anchor.observe();
    let _another_observation = observed;
    let _entry = super::resident_index::ResidentEntry::new(pin, 1);
    drop(anchor);
}
