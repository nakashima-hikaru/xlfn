// module: runtime
fn metadata_failure(runtime: &super::Runtime<()>) -> crate::XllResult<()> {
    super::residency::OpenResidency::acquire(runtime, std::ptr::null())?.preparation_failed();
    Ok(())
}
