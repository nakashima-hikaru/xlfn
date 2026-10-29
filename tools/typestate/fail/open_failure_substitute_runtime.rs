// module: runtime
use super::open::OpenFailure;
use super::{AddinLifecycleAccess, Runtime, UnloadPolicy};

fn invalid<A: crate::Addin>(failure: OpenFailure<'_, A>, other: &Runtime<A, impl UnloadPolicy<A>>, lifecycle: &AddinLifecycleAccess<'_, A>) {
    let _ = failure.recover(other, lifecycle); // error: E0061
}
