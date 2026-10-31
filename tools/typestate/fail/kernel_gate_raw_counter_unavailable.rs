// module: runtime
use xlfn_kernel::sealable_counter::SealableCounter; // error: E0603
fn invalid(_: &SealableCounter) {}
