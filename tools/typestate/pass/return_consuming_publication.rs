// module: return_abi
use super::ownership::{ReturnProducerGuard, ReturnPublicationTxn};
fn ordered(producer: ReturnProducerGuard<'static>, value: super::PreparedReturn) {
    let mut transaction = ReturnPublicationTxn::new(producer);
    let prepared = transaction.prepare(value);
    let _pointer = prepared.commit();
}
