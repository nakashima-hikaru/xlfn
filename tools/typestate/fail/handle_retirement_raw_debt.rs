// module: handle/domain
use super::retirement::RetirementAccounting;

fn invalid(accounting: &RetirementAccounting) {
    let _ = &accounting.debt; // error: E0616
}
