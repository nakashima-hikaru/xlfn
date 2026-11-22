// module: host/lib
// The raw ledger moved into the native adapter. A sibling cannot obtain its
// type; reclaiming a live charge must go through the affine public capability.
fn invalid(_: &super::process_quota::ProcessState) {} // error: E0603
