// module: handle/runtime
fn invalid(key: super::HandleTopicKey) -> super::HandleInitializationGuard<'static> {
    let service = super::FormulaHandleService::try_new(1).unwrap();
    super::HandleInitializationGuard::enter(&service, key).unwrap() // error: E0515
}
