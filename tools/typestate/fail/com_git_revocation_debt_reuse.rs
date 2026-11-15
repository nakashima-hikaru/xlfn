// module: excel_rtd/windows
use super::module_state::{ComModuleLifetime, GitRevocationDebtClaim};

fn resolve_twice(claim: GitRevocationDebtClaim<'_>) {
    claim.resolve();
    claim.resolve(); // error: E0382
}

fn resolve_after_requeue(claim: GitRevocationDebtClaim<'_>) {
    drop(claim);
    claim.resolve(); // error: E0382
}

fn detach_from_issuer() -> Vec<GitRevocationDebtClaim<'static>> {
    let module = ComModuleLifetime::new();
    module.claim_git_revocation_debt_batch() // error: E0515
}
