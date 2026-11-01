// module: return_abi
use super::ownership::ReturnProducerGuard;
fn invalid(producer: ReturnProducerGuard<'static>) {
    let _first = producer.transfer_to_block();
    let _second = producer.transfer_to_block(); // error: E0382
}
