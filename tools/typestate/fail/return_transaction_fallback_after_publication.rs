// module: return_abi
use super::ownership::ReturnPublicationTxn;
fn invalid(transaction: ReturnPublicationTxn) {
    let _owned = super::PreparedReturn::error(&crate::XllError::Panic).publish_owned_excel(transaction);
    let _fallback = super::PreparedReturn::error(&crate::XllError::Panic).publish_excel(transaction); // error: E0382
}
