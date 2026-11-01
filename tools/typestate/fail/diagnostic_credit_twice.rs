// module: diagnostics/worker
use super::QueuedCredit;
fn invalid(credit: QueuedCredit) {
    let delivery = credit.dequeue();
    let _again = credit.dequeue(); // error: E0382
    drop(delivery);
}
