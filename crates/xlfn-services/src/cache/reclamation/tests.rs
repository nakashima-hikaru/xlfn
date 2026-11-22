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
        retired.enqueue();
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
fn saturated_pending_weight_keeps_backpressure_until_both_queues_empty() {
    let domain = CacheLookupDomain::<()>::new();
    let reader = domain.domain.enter(0).unwrap();
    ReclaimEntry::sentinel(&domain, u64::MAX).enqueue();
    assert!(domain.poll_and_drain().is_none());
    assert_eq!(domain.domain.current_generation().index(), 1);
    ReclaimEntry::sentinel(&domain, u64::MAX).enqueue();
    drop(reader);

    let first = domain.poll_and_drain().unwrap().unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(domain.stats().pending_nodes, 1);
    assert_eq!(domain.pending_weight(), u64::MAX);

    let second = domain.quiesce_and_drain();
    assert_eq!(second.len(), 1);
    assert_eq!(domain.stats().pending_nodes, 0);
    assert_eq!(domain.pending_weight(), 0);
}

#[test]
#[cfg(not(all(target_os = "windows", target_arch = "x86")))]
#[cfg_attr(miri, ignore)]
fn loom_saturated_debt_resets_only_after_all_entries_are_accounted() {
    use loom::sync::{Arc, Mutex};

    loom::model(|| {
        let debt = Arc::new(Mutex::new(PendingDebt {
            nodes: 0,
            weight: Some(0),
        }));
        debt.lock().unwrap().enqueue(u64::MAX);
        debt.lock().unwrap().enqueue(1);

        let writer_debt = Arc::clone(&debt);
        let writer = loom::thread::spawn(move || writer_debt.lock().unwrap().enqueue(7));
        let first_debt = Arc::clone(&debt);
        let first = loom::thread::spawn(move || first_debt.lock().unwrap().retire(1, u64::MAX));
        debt.lock().unwrap().retire(1, 1);
        writer.join().unwrap();
        first.join().unwrap();

        let mut debt = debt.lock().unwrap();
        assert_eq!(debt.nodes, 1);
        // The concurrent insertion may have happened before the old batches
        // finished or after their debt was completely cleared.
        assert!(matches!(debt.weight, None | Some(7)));
        debt.retire(1, 7);
        assert_eq!(debt.nodes, 0);
        assert_eq!(debt.weight(), 0);
        debt.enqueue(9);
        assert_eq!(debt.weight(), 9);
    });
}

#[test]
fn nonblocking_drain_does_not_wait_for_detached_debt_owner() {
    let domain = CacheLookupDomain::<()>::new();
    ReclaimEntry::sentinel(&domain, 1).enqueue();
    // Model a blocking drain that has released the rotation lock but has not
    // yet accounted for its own batch.
    let detached = domain.domain.quiesce().unwrap();
    assert_eq!(domain.stats().pending_nodes, 1);
    let (tx, rx) = std::sync::mpsc::channel();
    let completed = std::thread::scope(|scope| {
        scope.spawn(|| {
            tx.send(domain.try_quiesce_and_drain().len()).unwrap();
        });
        let completed = rx.recv_timeout(std::time::Duration::from_secs(5));
        // Account even if the check timed out, so a failing test does not
        // strand the polling thread.
        for batch in detached.into_iter().flatten() {
            let _ = ReclaimEntries::account(&domain, batch.into_inner());
        }
        completed
    });
    assert_eq!(completed.unwrap(), 0);
    assert_eq!(domain.stats().pending_nodes, 0);
}

#[test]
fn nonblocking_drain_checks_next_generation_after_empty_rotation() {
    let domain = CacheLookupDomain::<()>::new();
    let reader = domain.domain.enter(0).unwrap();
    assert!(domain.poll_and_drain().is_none());
    ReclaimEntry::sentinel(&domain, 1).enqueue();
    drop(reader);

    let entries = domain.try_quiesce_and_drain();
    assert_eq!(entries.len(), 1);
    assert_eq!(domain.stats().pending_nodes, 0);
}
