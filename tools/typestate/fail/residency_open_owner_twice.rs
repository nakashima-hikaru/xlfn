// module: runtime
fn cannot_reissue(owner: super::residency::OpenResidency<'_, (), super::LogicalUnload>) {
    owner.preparation_failed();
    owner.preparation_failed(); // error: E0382
}
