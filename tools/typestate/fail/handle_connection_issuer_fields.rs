// module: excel_rtd/windows/server
fn invalid(runtime: &crate::handle::FormulaHandleService, generation: crate::handle::FormulaLifetimeGeneration, key: &str) {
    let mut connection = runtime.connect_transaction(generation, 1, key).unwrap();
    connection.token = key.to_owned(); // error: E0616
    drop(connection.disposition); // error: E0616
}
