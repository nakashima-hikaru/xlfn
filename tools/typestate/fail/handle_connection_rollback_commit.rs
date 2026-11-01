// module: excel_rtd/windows/server
fn invalid(runtime: &crate::handle::FormulaHandleService, generation: crate::handle::FormulaLifetimeGeneration, key: &str) {
    let connection = runtime.connect_transaction(generation, 1, key).unwrap();
    connection.rollback();
    let _ = connection.commit(); // error: E0382
}
