// module: subscription
use super::source::{RtdSource, SourceRegistration};

fn invalid<S, E>(registration: &SourceRegistration<E>, source: S)
where
    S: RtdSource,
    E: crate::RtdSubscription + From<S::Subscription>,
{
    let _ = registration.register(source); // error: E0277
}
