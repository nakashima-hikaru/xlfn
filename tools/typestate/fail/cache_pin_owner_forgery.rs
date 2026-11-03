// module: cache
fn invalid<V>(node: super::NodePtr<V>) {
    let _ = super::pins::ResidentPin { node }; // error: E0451
    let _ = super::pins::FlightAnchor { node }; // error: E0451
}
