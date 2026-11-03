// module: return_abi/numeric
fn invalid(state: &super::process::ProcessState, bytes: usize) {
    state.release_published(bytes); // error: E0624
}
