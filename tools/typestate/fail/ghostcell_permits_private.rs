// module: value
fn cannot_remove(storage: &crate::call::CallStorage<'_, '_>) {
    let _ = &storage.handle_permits; // error: E0616
}
