// module: handle
fn invalid(publication: super::publication::InsertedPublication<'_>) {
    let other = super::PublishedTopic::new("other".into(), "other-lifetime".into());
    let _ = publication.publish_and_observe(other, |_, _| {}); // error: E0277
}
