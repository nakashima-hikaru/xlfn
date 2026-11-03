# use xlfn::prelude::*;
# use xlfn::cache::{CacheEndpoint, CacheRegistry};
# static UPPERCASE: CacheEndpoint<String, String> = CacheEndpoint::new("uppercase-v1");
# struct State { caches: CacheRegistry }
# let state = State { caches: CacheRegistry::new(1024) };
# let text = String::from("example");
