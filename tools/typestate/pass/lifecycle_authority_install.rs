// module: lifecycle/state
use crate::module_runtime::ModuleAuthority;

fn valid<A: crate::Addin>(
    state: super::LifecycleState<A>,
    authority: ModuleAuthority,
) -> Result<super::LifecycleState<A>, (super::LifecycleState<A>, ModuleAuthority)> {
    super::PreparedAuthorityInstall::acquire(state, authority).map(|prepared| prepared.install())
}
