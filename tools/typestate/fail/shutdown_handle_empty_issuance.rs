// module: runtime
fn invalid() {
    let _ = crate::shutdown::HandlesSealed::empty(None); // error: E0599
    let _ = crate::shutdown::HandlesQuiescent::new(None); // error: E0599
}
