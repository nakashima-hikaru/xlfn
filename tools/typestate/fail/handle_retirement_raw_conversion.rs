// module: handle/domain
use super::HandleReadDomain;

fn invalid(domain: &HandleReadDomain) {
    let _ = domain.take_generation(); // error: E0599
}
