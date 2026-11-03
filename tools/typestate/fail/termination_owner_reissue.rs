// module: subscription
use super::termination::TerminationOwner;
fn invalid(owner: TerminationOwner<'_>) -> crate::XllResult<()> {
    owner.complete(Ok(()))?;
    owner.complete(Ok(())) // error: E0382
}
