// module: subscription
use super::source::{RtdSource, SourceRegistration};

fn invalid<S: RtdSource>(
    registration: &SourceRegistration<S::Subscription, (S,)>,
    source: S,
) {
    let _ = registration.register(source); // error: E0599
}
