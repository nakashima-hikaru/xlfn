// module: handle
fn ordered(publication: super::publication::InsertedPublication<'_>) -> crate::XllResult<String> {
    let _observed_metadata = (publication.token(), publication.allocation());
    publication.publish_and_observe(|_, _| Ok(()), |_, _| {})?.commit()
}
