// module: handle
fn invalid(publication: super::publication::InsertedPublication<'_>) {
    let _transaction = publication.transaction; // error: E0616
}
