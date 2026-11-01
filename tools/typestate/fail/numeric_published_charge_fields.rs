// module: return_abi/numeric
fn invalid(charge: super::process::PublishedCharge, bytes: usize) {
    let _ = charge.state; // error: E0616
    let _ = charge.bytes; // error: E0616
    let _ = bytes;
}
