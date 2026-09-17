use xlfn::cache::{CacheEndpoint, CacheLease, CacheRegistry};

enum Prices {}

static PRICES: CacheEndpoint<Prices, String, f64> = CacheEndpoint::new("prices-v1");

#[allow(dead_code)]
fn lookup<'a>(registry: &'a CacheRegistry, key: String) -> xlfn::XllResult<CacheLease<'a, f64>> {
    let cache = registry.bind(&PRICES)?;
    cache.get_or_try_insert(key, |_| 1, || Ok(42.0))
}

fn main() {}
