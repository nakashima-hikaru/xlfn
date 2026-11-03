// module: return_abi
use super::ownership::{ReturnProducerGuard, ReturnPublicationTxn};
fn invalid(producer: ReturnProducerGuard<'static>) {
    let _block = producer.transfer_to_block();
    let _fallback = ReturnPublicationTxn::new(producer); // error: E0382
}
