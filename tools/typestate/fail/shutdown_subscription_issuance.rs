// module: runtime
fn invalid() {
    let _ = crate::shutdown::SubscriptionsStopped::issue(None); // error: E0599
}
