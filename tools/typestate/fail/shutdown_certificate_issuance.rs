// module: runtime
fn invalid() {
    let _ = crate::shutdown::AddinQuiesced::issue(); // error: E0624
    let _ = crate::shutdown::GenerationReclaimed::issue(); // error: E0624
    let _ = crate::shutdown::GenerationCallbacksDetached::issue(); // error: E0624
}
