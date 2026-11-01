// module: async_udf
fn invalid() {
    let _ = super::registry::TaskRegistrationId(1); // error: E0603
}
