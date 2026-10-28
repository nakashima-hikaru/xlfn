// module: subscription
use super::source::{RtdSource, SourceRegistration};

fn valid<S, E>(registration: &SourceRegistration<E>, source: S)
where
    S: RtdSource,
    E: crate::RtdSubscription,
    S::Subscription: crate::IntoRtdSubscription<E>,
{
    let _ = registration.register(source);
}
