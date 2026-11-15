// module: excel_rtd/windows/server
fn invalid(runtime: &crate::handle::FormulaHandleService, generation: crate::handle::FormulaLifetimeGeneration, key: &str) {
    let connection = runtime.connect_transaction(generation, 1, key).unwrap();
    drop(connection);
    let _ = connection.commit(); // error: E0382
}

fn cannot_rollback_after_commit(runtime: &crate::handle::FormulaHandleService, generation: crate::handle::FormulaLifetimeGeneration, key: &str) {
    let connection = runtime.connect_transaction(generation, 1, key).unwrap();
    let _ = connection.commit();
    drop(connection); // error: E0382
}
