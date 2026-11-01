// module: cache
fn invalid<V>(domain: &super::CacheLookupDomain<V>) {
    let closed = domain.seal();
    let _first = closed.drain();
    let _second = closed.drain(); // error: E0382
}
