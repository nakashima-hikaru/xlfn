// module: async_udf
use super::registry::TaskRegistrationId;
fn consume(_: TaskRegistrationId) {}
fn invalid(id: TaskRegistrationId) {
    consume(id);
    consume(id); // error: E0382
}
