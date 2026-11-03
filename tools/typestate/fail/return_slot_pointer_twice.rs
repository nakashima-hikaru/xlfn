// module: return_abi
use super::slot::PublishedSlot;
fn invalid(published: PublishedSlot<'_>) {
    let _first = published.into_pointer();
    let _second = published.into_pointer(); // error: E0382
}
