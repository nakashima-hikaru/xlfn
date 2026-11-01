// module: return_abi
use super::ownership::ReturnPublicationTxn;
fn invalid(transaction: ReturnPublicationTxn) {
    let _first = transaction.prepare(super::PreparedReturn::error(&crate::XllError::Panic));
    let _second = transaction.prepare(super::PreparedReturn::error(&crate::XllError::Panic)); // error: E0382
}
