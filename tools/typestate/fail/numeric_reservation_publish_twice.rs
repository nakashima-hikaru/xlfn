// module: return_abi/numeric
fn invalid() -> crate::XllResult<()> {
    let reserved = super::process::Reservation::reserve(64)?;
    let _published = reserved.publish();
    let _again = reserved.publish(); // error: E0382
    Ok(())
}
