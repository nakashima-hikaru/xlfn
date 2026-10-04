use xlfn::cache::{CacheEndpoint, CacheLease, CacheRegistry};

static PRICES: CacheEndpoint<String, f64> = CacheEndpoint::new("prices-v1");

#[allow(dead_code)]
fn lookup<'a>(registry: &'a CacheRegistry, key: String) -> xlfn::XllResult<CacheLease<'a, f64>> {
    registry.get_or_try_insert(&PRICES, key, |_| 1, || Ok(42.0))
}

fn main() {}

