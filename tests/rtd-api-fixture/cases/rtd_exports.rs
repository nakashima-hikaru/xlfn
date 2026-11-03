use framework::{DynamicRtdSources, IntoRtdSubscription, RtdSourceCatalog, RtdSubscription};

fn subscription<T: RtdSubscription + IntoRtdSubscription<T>>() {}
fn catalog<T: RtdSourceCatalog<()>>() {}

pub fn exports_are_available() {
    subscription::<()>();
    catalog::<DynamicRtdSources<()>>();
}
