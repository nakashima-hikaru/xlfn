// module: runtime/shutdown/pipeline
use super::{ModuleAuthorityRecovery, ServicesCleaned, ServicesSealed};
use crate::runtime::capabilities::{EmptyLifecycle, InstalledLifecycle};
fn before<'a, A: crate::Addin>(binding: <ServicesSealed<'a, A> as ModuleAuthorityRecovery>::Binding<'a, A>) -> Option<InstalledLifecycle<'a, A>> {
    binding
}
fn after<'a, A: crate::Addin>(binding: <ServicesCleaned as ModuleAuthorityRecovery>::Binding<'a, A>) -> EmptyLifecycle<'a, A> {
    binding
}
