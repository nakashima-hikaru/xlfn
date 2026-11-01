use super::*;

struct PayloadDrop(std::sync::Arc<AtomicUsize>);

impl Drop for PayloadDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}

#[test]
#[allow(
    clippy::drop_non_drop,
    clippy::forget_non_drop,
    reason = "fault injection checks abandoning an affine close proof, including no Drop guarantee"
)]
fn abandoning_closed_cache_owner_retains_payload_until_its_own_drain() {
    for forget in [false, true] {
        let drops = std::sync::Arc::new(AtomicUsize::new(0));
        let domain = Box::new(CacheLookupDomain::new());
        let node = Box::into_non_null(Box::new(CacheNode {
            value: PayloadDrop(std::sync::Arc::clone(&drops)),
            pins: AtomicU32::new(1),
            resident: AtomicBool::new(false),
            published: true,
            weight: 1,
            generation: 0,
            domain: NonNull::from(&*domain),
        }));
        // SAFETY: this fixture owns the allocation's sole creation pin and
        // retains the boxed domain through final recovery. Enqueue separately
        // so ordinary CacheLease Drop cannot drive maintenance before seal.
        let retired = unsafe { ReclaimEntry::release_pin(node) }.unwrap();
        domain.enqueue_reclaim(retired);
        assert_eq!(domain.pending_nodes.load(Ordering::Acquire), 1);
        let closed = domain.seal();
        if forget {
            std::mem::forget(closed);
        } else {
            drop(closed);
        }
        assert_eq!(drops.load(Ordering::Acquire), 0);
        let retired = domain.seal().drain();
        assert_eq!(retired.len(), 1);
        reclaim_cache_entries(retired);
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert!(domain.seal().drain().is_empty());
    }
}

#[test]
#[cfg(not(miri))]
fn foreign_cache_reclaim_batch_is_fail_stop() {
    const CASE: &str = "XLFN_TEST_FOREIGN_CACHE_RECLAIM_BATCH";
    if let Ok(case) = std::env::var(CASE) {
        let owner = CacheLookupDomain::<()>::new();
        let foreign = CacheLookupDomain::<()>::new();
        if case == "register" {
            owner.enqueue_reclaim_with_hook(ReclaimEntry::sentinel(&foreign, 1), |_| {
                std::process::exit(86); // A distinct failure if registration is reached.
            });
            return;
        }
        foreign.enqueue_reclaim(ReclaimEntry::sentinel(&foreign, 1));
        let foreign_batch = foreign.quiesce_and_drain();
        match case.as_str() {
            "first" => {
                let _ = merge_reclaims(&owner, [foreign_batch]);
            }
            "append" => {
                owner.enqueue_reclaim(ReclaimEntry::sentinel(&owner, 2));
                let own_batch = owner.quiesce_and_drain();
                let _ = merge_reclaims(&owner, [own_batch, foreign_batch]);
            }
            _ => panic!("unknown foreign-batch test case"),
        }
        return;
    }
    for case in ["first", "append", "register"] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cache::reclamation::tests::foreign_cache_reclaim_batch_is_fail_stop",
            ])
            .env(CASE, case)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "foreign batch must fail-stop: {case}");
        assert_ne!(status.code(), Some(86), "registration ran before rejection");
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(6), "expected SIGABRT: {case}");
        }
    }
}

#[test]
fn drained_cache_batches_keep_owner_and_all_entries_when_merged() {
    let domain = CacheLookupDomain::<()>::new();
    domain.enqueue_reclaim(ReclaimEntry::sentinel(&domain, 11));
    let first = domain.quiesce_and_drain();
    domain.enqueue_reclaim(ReclaimEntry::sentinel(&domain, 17));
    let second = domain.quiesce_and_drain();
    let batch = merge_reclaims(&domain, [first, second]);
    assert!(batch.belongs_to(&domain));
    assert_eq!(batch.len(), 2);
    assert_eq!(batch.iter().map(|entry| entry.weight).sum::<u64>(), 28);
    assert_eq!(domain.pending_nodes.load(Ordering::Relaxed), 0);
    // Sentinel entries exercise queue ownership only, never destruction.
}

#[test]
fn saturated_pending_weight_keeps_backpressure_until_both_queues_empty() {
    let domain = CacheLookupDomain::<()>::new();
    let reader = domain.domain.enter(0).unwrap();
    domain.enqueue_reclaim(ReclaimEntry::sentinel(&domain, u64::MAX));
    assert!(
        domain
            .domain
            .poll_quiesce()
            .map(|result| result.map(|batch| domain.drain_generation(batch)))
            .is_none()
    );
    assert_eq!(domain.domain.current_generation().index(), 1);
    domain.enqueue_reclaim(ReclaimEntry::sentinel(&domain, u64::MAX));
    drop(reader);

    let first = domain
        .domain
        .poll_quiesce()
        .map(|result| result.map(|batch| domain.drain_generation(batch)))
        .unwrap()
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(domain.stats().pending_nodes, 1);
    assert_eq!(domain.pending_weight(), u64::MAX);

    let second = domain.quiesce_and_drain();
    assert_eq!(second.len(), 1);
    assert_eq!(domain.stats().pending_nodes, 0);
    assert_eq!(domain.pending_weight(), 0);
}
