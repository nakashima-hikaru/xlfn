//! Bounded, non-owning memoization of endpoint resolution.
//!
//! Each pointer belongs to a registry whose identity is never reused. Entries
//! may outlive that registry in TLS, but lookup only returns a pointer for the
//! identity of a currently borrowed registry. The registry retains its boxed
//! caches until destruction; moving or clearing it does not move those boxes.
//! No cache operation or application callback runs while TLS is borrowed.

use std::any::TypeId;
use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

const SET_BITS: u32 = 5;
const SET_COUNT: usize = 1 << SET_BITS;
const WAYS: usize = 2;
static NEXT_REGISTRY_IDENTITY: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RegistryIdentity(u64);

impl RegistryIdentity {
    pub(super) fn fresh() -> Self {
        Self(
            NEXT_REGISTRY_IDENTITY
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop()),
        )
    }
}

#[derive(Clone, Copy)]
struct ResolvedEndpoint {
    registry: RegistryIdentity,
    key: (TypeId, &'static str),
    pointer: NonNull<()>,
}

struct EndpointCache {
    sets: [[Option<ResolvedEndpoint>; WAYS]; SET_COUNT],
    victims: [u8; SET_COUNT],
}

thread_local! {
    static ENDPOINTS: RefCell<EndpointCache> = const { RefCell::new(EndpointCache {
        sets: [[None; WAYS]; SET_COUNT],
        victims: [0; SET_COUNT],
    }) };
}

#[inline]
fn set_index(registry: RegistryIdentity, id: &'static str) -> usize {
    // Addresses select candidates only; the complete typed key and registry
    // identity still authorize every hit. Equal strings at different addresses
    // may miss, then resolve to the same registry-owned cache through the map.
    // Multiplication spreads aligned and adjacent string addresses without
    // hashing their bytes. Use u64 arithmetic on both 32- and 64-bit targets.
    let address = id.as_ptr().addr() as u64;
    let mixed = (address ^ registry.0).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    (mixed >> (u64::BITS - SET_BITS)) as usize
}

pub(super) fn lookup(
    registry: RegistryIdentity,
    key: (TypeId, &'static str),
) -> Option<NonNull<()>> {
    let index = set_index(registry, key.1);
    ENDPOINTS
        .try_with(|cache| {
            cache.borrow().sets[index]
                .iter()
                .flatten()
                .find_map(|entry| {
                    (entry.registry == registry && entry.key == key).then_some(entry.pointer)
                })
        })
        .ok()
        .flatten()
}

pub(super) fn remember(
    registry: RegistryIdentity,
    key: (TypeId, &'static str),
    pointer: NonNull<()>,
) {
    let index = set_index(registry, key.1);
    // During TLS teardown, the ordinary registry lookup remains available.
    let _ = ENDPOINTS.try_with(|cache| {
        let mut cache = cache.borrow_mut();
        let victim = usize::from(cache.victims[index]);
        cache.sets[index][victim] = Some(ResolvedEndpoint {
            registry,
            key,
            pointer,
        });
        cache.victims[index] = ((victim + 1) % WAYS) as u8;
    });
}

#[cfg(test)]
mod tests {
    use super::super::{CacheEndpoint, CacheRegistry};
    use std::time::Duration;

    #[test]
    fn warm_endpoint_resolution_does_not_take_the_registry_lock() {
        let registry = CacheRegistry::new(64);
        let endpoint = CacheEndpoint::<u64, u64>::new("warm-lock-free-resolution");
        std::thread::scope(|scope| {
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (start_tx, start_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let registry = &registry;
            let worker = scope.spawn(move || {
                drop(
                    endpoint
                        .get_or_try_insert(registry, 1, |_| 1, || Ok(7))
                        .unwrap(),
                );
                ready_tx.send(()).unwrap();
                start_rx.recv().unwrap();
                for _ in 0..100 {
                    assert_eq!(*endpoint.get(registry, &1).unwrap().unwrap(), 7);
                }
                done_tx.send(()).unwrap();
            });
            ready_rx.recv().unwrap();
            let lock = registry.caches.write();
            start_tx.send(()).unwrap();
            let completed = done_rx.recv_timeout(Duration::from_secs(5));
            // Release before checking so a regression cannot strand the worker.
            drop(lock);
            worker.join().unwrap();
            completed.expect("warm endpoint resolution must bypass the registry lock");
        });
    }

    #[test]
    fn miri_registry_endpoint_resolution_keeps_owner_identity() {
        let endpoint = CacheEndpoint::<u64, String>::new("registry-lifetime");
        let first = CacheRegistry::new(64);
        drop(
            endpoint
                .get_or_try_insert(&first, 1, String::len, || Ok("old".into()))
                .unwrap(),
        );
        let identity = first.identity;
        // The box allocation remains stable when the registry owner moves.
        let mut registry = first;
        assert_eq!(registry.identity, identity);
        assert_eq!(&*endpoint.get(&registry, &1).unwrap().unwrap(), "old");
        registry.clear();
        assert!(endpoint.get(&registry, &1).unwrap().is_none());
        drop(
            endpoint
                .get_or_try_insert(&registry, 1, String::len, || Ok("new".into()))
                .unwrap(),
        );
        assert_eq!(&*endpoint.get(&registry, &1).unwrap().unwrap(), "new");

        // Replace in the same stack slot: stale TLS pointers cannot authorize
        // access to the destroyed registry even if allocator addresses repeat.
        registry = CacheRegistry::new(64);
        assert_ne!(registry.identity, identity);
        assert!(endpoint.get(&registry, &1).unwrap().is_none());
        drop(
            endpoint
                .get_or_try_insert(&registry, 1, String::len, || Ok("replacement".into()))
                .unwrap(),
        );
        assert_eq!(
            &*endpoint.get(&registry, &1).unwrap().unwrap(),
            "replacement"
        );
    }

    #[test]
    fn miri_endpoint_cache_preserves_registry_and_type_separation() {
        enum Marker {}
        let first = CacheRegistry::new(64);
        let second = CacheRegistry::new(64);
        let numbers = CacheEndpoint::<u64, u64>::new("same-name");
        let marked = CacheEndpoint::<u64, u64, Marker>::new("same-name");
        let strings = CacheEndpoint::<u64, String>::new("same-name");
        drop(
            numbers
                .get_or_try_insert(&first, 1, |_| 1, || Ok(7))
                .unwrap(),
        );
        drop(
            numbers
                .get_or_try_insert(&second, 1, |_| 1, || Ok(9))
                .unwrap(),
        );
        drop(
            marked
                .get_or_try_insert(&first, 1, |_| 1, || Ok(11))
                .unwrap(),
        );
        drop(
            strings
                .get_or_try_insert(&first, 1, String::len, || Ok("text".into()))
                .unwrap(),
        );
        for _ in 0..3 {
            assert_eq!(*numbers.get(&first, &1).unwrap().unwrap(), 7);
            assert_eq!(*numbers.get(&second, &1).unwrap().unwrap(), 9);
            assert_eq!(*marked.get(&first, &1).unwrap().unwrap(), 11);
            assert_eq!(&*strings.get(&first, &1).unwrap().unwrap(), "text");
        }
    }

    #[test]
    fn more_endpoints_than_tls_capacity_fall_back_without_losing_values() {
        let registry = CacheRegistry::new(64);
        const IDS: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-_.";
        assert!(IDS.len() > super::SET_COUNT * super::WAYS);
        let ids: Vec<_> = (0..IDS.len()).map(|index| &IDS[index..index + 1]).collect();
        for (value, id) in ids.iter().enumerate() {
            let endpoint = CacheEndpoint::<u64, u64>::new(id);
            drop(
                endpoint
                    .get_or_try_insert(&registry, 1, |_| 1, || Ok(value as u64))
                    .unwrap(),
            );
        }
        for _ in 0..2 {
            for (value, id) in ids.iter().enumerate() {
                let endpoint = CacheEndpoint::<u64, u64>::new(id);
                assert_eq!(*endpoint.get(&registry, &1).unwrap().unwrap(), value as u64);
            }
        }
    }

    #[test]
    fn miri_equal_endpoint_names_share_storage_across_resolution_sets() {
        static NAMES: [u8; 128] = [b'x'; 128];
        let registry = CacheRegistry::new(64);
        let first_name = std::str::from_utf8(&NAMES[..8]).unwrap();
        let first_set = super::set_index(registry.identity, first_name);
        let second_name = (1..=NAMES.len() - 8)
            .map(|start| std::str::from_utf8(&NAMES[start..start + 8]).unwrap())
            .find(|name| super::set_index(registry.identity, name) != first_set)
            .expect("adjacent string addresses exercise different resolution sets");
        assert_eq!(first_name, second_name);
        let first = CacheEndpoint::<u64, u64>::new(first_name);
        let second = CacheEndpoint::<u64, u64>::new(second_name);
        drop(
            first
                .get_or_try_insert(&registry, 1, |_| 1, || Ok(7))
                .unwrap(),
        );
        assert_eq!(*second.get(&registry, &1).unwrap().unwrap(), 7);
        assert_eq!(registry.endpoint_count(), 1);
        registry.clear();
        assert!(first.get(&registry, &1).unwrap().is_none());
        assert!(second.get(&registry, &1).unwrap().is_none());
    }
}
