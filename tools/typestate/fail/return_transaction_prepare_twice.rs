// module: return_abi
use super::ownership::ReturnPublicationTxn;
fn invalid(transaction: ReturnPublicationTxn) {
    let _first = transaction.prepare(super::PreparedReturn::error(&crate::XllError::PANIC));
    let _second = transaction.prepare(super::PreparedReturn::error(&crate::XllError::PANIC)); // error: E0382
}
