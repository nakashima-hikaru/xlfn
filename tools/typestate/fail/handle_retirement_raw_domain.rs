// module: handle/domain
use super::retirement::RetirementAccounting;

fn invalid(accounting: &RetirementAccounting) {
    let _ = &accounting.domain; // error: E0616
}
