// module: runtime
fn invalid() {
    let _ = crate::shutdown::AddinQuiesced::issue(); // error: E0624
    let _ = crate::shutdown::GenerationReclaimed::issue(); // error: E0624
    let _ = crate::shutdown::GenerationCallbacksDetached::issue(); // error: E0624
    let _ = crate::shutdown::HandlesSealed::empty(None); // error: E0599
    let _ = crate::shutdown::HandlesQuiescent::new(None); // error: E0599
    let _ = crate::shutdown::SubscriptionsStopped::issue(None); // error: E0599
}
