// module: return_abi
use super::slot::FreeingSlot;
fn invalid(freeing: FreeingSlot<'_>) {
    let _first = freeing.destroy();
    let _second = freeing.destroy(); // error: E0382
}
