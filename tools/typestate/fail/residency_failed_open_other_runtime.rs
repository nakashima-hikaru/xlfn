// module: runtime
fn cannot_inject_target(
    owner: super::residency::OpenResidency<'_, (), super::LogicalUnload>,
    other: &super::Runtime<()>,
) {
    owner.preparation_failed(other); // error: E0061
}
