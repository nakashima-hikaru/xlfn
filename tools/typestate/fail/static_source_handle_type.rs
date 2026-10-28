// module: subscription
use super::source::{RtdSource, RtdSourceHandle, SourceRegistration};

fn invalid<S: RtdSource, T: RtdSource>(
    registration: &SourceRegistration<S::Subscription, (S,)>,
    source: S,
) -> crate::XllResult<RtdSourceHandle<T>> {
    let (handle,) = registration.register_sources((source,))?;
    Ok(handle) // error: E0308
}
