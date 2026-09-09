//! Removal latency and final-drain accounting, excluded from production builds.
use super::ExcelHandleObject;
use super::registry::HandleRegistry;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

struct Payload(Arc<AtomicUsize>);
impl ExcelHandleObject for Payload {}
impl Drop for Payload {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn handle_removal_probe(samples: usize, reader_hold: Duration) -> serde_json::Value {
    assert!(samples > 0);
    let drops = Arc::new(AtomicUsize::new(0));
    let mut remove = Vec::with_capacity(samples);
    let mut drain = Vec::with_capacity(samples);
    let mut retained_after_remove = 0;
    for index in 0..samples {
        let registry = Arc::new(HandleRegistry::try_new(1).unwrap());
        let pending = registry.new_object(Payload(Arc::clone(&drops))).unwrap();
        let (token, ..) = registry.publish_pending::<Payload>(pending).unwrap();
        let reader = if reader_hold.is_zero() {
            None
        } else {
            let reader_registry = Arc::clone(&registry);
            let reader_token = token.clone();
            let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
            let worker = std::thread::spawn(move || {
                crate::call::with_excel_call_scope_and_state(
                    &reader_registry,
                    |registry, scope| {
                        let borrowed = registry
                            .lookup_handle::<Payload>(scope, &reader_token)
                            .unwrap();
                        ready_tx.send(()).unwrap();
                        std::thread::sleep(reader_hold);
                        std::hint::black_box(&*borrowed);
                    },
                );
            });
            ready_rx.recv().unwrap();
            Some(worker)
        };
        let started = Instant::now();
        assert!(
            registry
                .remove_and_drop_with_observer(&token, "benchmark removal", |_| {})
                .is_some()
        );
        remove.push(started.elapsed().as_nanos() as u64);
        retained_after_remove += usize::from(drops.load(Ordering::Relaxed) == index);
        let started = Instant::now();
        let sealed = registry.seal().unwrap();
        registry.finish_quiescence(&sealed).unwrap();
        drain.push(started.elapsed().as_nanos() as u64);
        if let Some(reader) = reader {
            reader.join().unwrap();
        }
        assert_eq!(drops.load(Ordering::Relaxed), index + 1);
    }
    remove.sort_unstable();
    drain.sort_unstable();
    let percentile =
        |values: &[u64], percent: usize| values[(values.len() * percent).div_ceil(100) - 1];
    serde_json::json!({
        "samples": samples, "reader_hold_us": reader_hold.as_micros(),
        "remove_p50_ns": percentile(&remove, 50), "remove_p99_ns": percentile(&remove, 99),
        "final_drain_p50_ns": percentile(&drain, 50), "final_drain_p99_ns": percentile(&drain, 99),
        "retained_after_remove": retained_after_remove, "destroyed_after_final_drain": drops.load(Ordering::Relaxed),
    })
}

pub fn handle_retirement_debt_probe() -> serde_json::Value {
    let registry = HandleRegistry::from_entropy(8, [7; 40]);
    let drops = Arc::new(AtomicUsize::new(0));
    let publish = || {
        let pending = registry.new_object(Payload(Arc::clone(&drops))).unwrap();
        registry
            .publish_pending::<Payload>(pending)
            .map(|(token, ..)| token)
    };
    let mut live: Vec<_> = (0..8).map(|_| publish().unwrap()).collect();
    let (queued, debt, peak) =
        crate::call::with_excel_call_scope_and_state(&registry, |registry, scope| {
            let borrowed = registry.lookup_handle::<Payload>(scope, &live[0]).unwrap();
            registry
                .remove_and_drop_with_observer(&live.remove(0), "debt probe", |_| {})
                .unwrap();
            for _ in 1..super::domain::HARD_DEBT_LIMIT {
                let token = publish().unwrap();
                registry
                    .remove_and_drop_with_observer(&token, "debt probe", |_| {})
                    .unwrap();
            }
            // Failed publication owns one transient payload, which is dropped
            // immediately. It is not a retired binding or part of retirement debt.
            assert!(matches!(publish(), Err(crate::XllError::Overloaded)));
            for token in live {
                registry
                    .remove_and_drop_with_observer(&token, "debt probe remaining live", |_| {})
                    .unwrap();
            }
            assert_eq!(borrowed.0.load(Ordering::Relaxed), 1);
            registry.bindings.read_domain().debt_snapshot()
        });
    assert_eq!(debt, super::domain::HARD_DEBT_LIMIT + 7);
    let started = Instant::now();
    let sealed = registry.seal().unwrap();
    registry.finish_quiescence(&sealed).unwrap();
    let drain_ns = started.elapsed().as_nanos();
    assert_eq!(registry.bindings.read_domain().debt(), 0);
    assert_eq!(drops.load(Ordering::Relaxed), debt + 1);
    serde_json::json!({ "soft_threshold": super::domain::SOFT_DEBT_LIMIT,
        "hard_threshold": super::domain::HARD_DEBT_LIMIT, "maximum_live_bindings": 8,
        "queued_while_own_scope_live": queued, "debt_while_own_scope_live": debt,
        "peak_debt": peak, "final_debt": 0, "payloads_dropped": debt + 1,
        "final_drain_ns": drain_ns })
}
