// module: runtime
fn valid(protocol: &crate::runtime_components::ReturnProtocol) -> crate::XllResult<bool> {
    let drained = protocol.close_admission().wait()?;
    Ok(drained.matches(protocol))
}
