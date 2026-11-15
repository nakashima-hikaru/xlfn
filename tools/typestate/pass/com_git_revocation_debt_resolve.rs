// module: excel_rtd/windows
use super::module_state::GitRevocationDebtClaim;
use crate::XllResult;
use std::num::NonZeroU32;

fn retry(claim: GitRevocationDebtClaim<'_>, revoke: impl FnOnce(NonZeroU32) -> XllResult<()>) -> XllResult<()> {
    revoke(claim.cookie())?;
    claim.resolve();
    Ok(())
}
