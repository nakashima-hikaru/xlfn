// module: return_abi/numeric
fn publish() -> crate::XllResult<super::process::PublishedCharge> {
    Ok(super::process::Reservation::reserve(64)?.publish())
}
