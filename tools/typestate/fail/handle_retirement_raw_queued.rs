// module: handle/domain
use super::retirement::RetirementAccounting;

fn invalid(accounting: &RetirementAccounting) {
    let _ = &accounting.queued; // error: E0616
}
