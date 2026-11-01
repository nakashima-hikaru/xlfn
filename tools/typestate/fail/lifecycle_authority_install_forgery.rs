// module: lifecycle/state
fn invalid<A: crate::Addin>(prepared: super::PreparedAuthorityInstall<A>) {
    let _ = super::PreparedAuthorityInstall { ..prepared }; // error: E0451
}
