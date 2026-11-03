// module: cache
fn invalid<V>(pin: super::pins::ResidentPin<V>) {
    pin.retire();
    pin.retire(); // error: E0382
}
