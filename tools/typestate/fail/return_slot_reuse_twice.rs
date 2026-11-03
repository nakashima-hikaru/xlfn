// module: return_abi
use super::slot::ReusableSlot;
fn invalid(reusable: ReusableSlot<'_>) {
    reusable.release();
    reusable.release(); // error: E0382
}
