// module: excel_rtd/windows/server
fn invalid(runtime: &crate::handle::FormulaHandleService, generation: crate::handle::FormulaLifetimeGeneration, key: &str) {
    let mut connection = runtime.connect_transaction(generation, 1, key).unwrap();
    connection.runtime = runtime; // error: E0616
    let _ = connection.disposition.take(); // error: E0616
}
