// module: return_abi
use super::ownership::ReturnProducerGuard;
fn invalid(producer: ReturnProducerGuard<'static>) {
    let _ = producer.obligation; // error: E0616
}
