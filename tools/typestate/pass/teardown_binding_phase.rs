// module: runtime/shutdown/pipeline
use super::{BindingPhase, ServicesCleaned, ServicesSealed};
use crate::runtime::capabilities::{EmptyLifecycle, InstalledLifecycle};
fn before<'a, A: crate::Addin>(binding: <ServicesSealed<'a, A> as BindingPhase>::Binding<'a, A>) -> Option<InstalledLifecycle<'a, A>> {
    binding
}
fn after<'a, A: crate::Addin>(binding: <ServicesCleaned as BindingPhase>::Binding<'a, A>) -> EmptyLifecycle<'a, A> {
    binding
}
