// module: subscription
use super::source::{RtdSource, RtdSourceHandle, SourceRegistration};

fn valid<S: RtdSource>(
    registration: &SourceRegistration<S::Subscription, (S,)>,
    source: S,
) -> crate::XllResult<RtdSourceHandle<S>> {
    let (handle,): (RtdSourceHandle<S>,) = registration.register_sources((source,))?;
    Ok(handle)
}
