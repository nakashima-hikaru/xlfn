//! Public source-construction allocation probe; no subscriptions or workers.
//! Retention-vector storage, warmup, assertions, reporting, and drops are excluded.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use xlfn::XllResult;
use xlfn::rtd::{
    RtdCapacity, RtdChannelSource, RtdDeliveryPolicy, RtdFifo, RtdLatest, RtdSender,
    RtdSourceLimits, RtdTopic,
};

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static DEALLOCS: Cell<usize> = const { Cell::new(0) };
    static REALLOCS: Cell<usize> = const { Cell::new(0) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;
// SAFETY: pointers/layouts pass unchanged to System; TLS counters do not allocate.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.try_with(Cell::get).unwrap_or(false) {
            ALLOCS.set(ALLOCS.get() + 1);
            BYTES.set(BYTES.get() + layout.size());
        }
        // SAFETY: the caller supplies a valid allocation layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if ENABLED.try_with(Cell::get).unwrap_or(false) {
            DEALLOCS.set(DEALLOCS.get() + 1);
        }
        // SAFETY: the caller supplies a live System allocation and its layout.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.try_with(Cell::get).unwrap_or(false) {
            REALLOCS.set(REALLOCS.get() + 1);
            BYTES.set(BYTES.get() + size);
        }
        // SAFETY: the caller supplies a live allocation and valid new size.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

static FACTORY_CALLS: AtomicUsize = AtomicUsize::new(0);
type Producer<D> = fn(RtdSender<f64, D>) -> XllResult<()>;
type Factory<D> = fn(RtdTopic) -> XllResult<Producer<D>>;
type Source<D> = RtdChannelSource<f64, Factory<D>, D>;

fn producer<D: RtdDeliveryPolicy>(_: RtdSender<f64, D>) -> XllResult<()> {
    unreachable!("source construction must not start a producer")
}

fn factory<D: RtdDeliveryPolicy>(_: RtdTopic) -> XllResult<Producer<D>> {
    FACTORY_CALLS.fetch_add(1, Ordering::Relaxed);
    Ok(producer::<D>)
}

fn limits(configured: bool) -> RtdSourceLimits {
    if configured {
        RtdSourceLimits::standard().with_max_subscriptions(RtdCapacity::disabled_if_zero(11))
    } else {
        RtdSourceLimits::standard()
    }
}

fn construct<D: RtdDeliveryPolicy>(delivery: D, configured: bool) -> Source<D> {
    let source = RtdChannelSource::with_policy(delivery, factory::<D> as Factory<D>);
    if configured {
        source
            .with_max_producers(NonZeroUsize::new(7).unwrap())
            .with_limits(limits(configured))
    } else {
        source
    }
}

fn run<D: RtdDeliveryPolicy>(delivery: D, configured: bool) {
    let report_only = std::env::var_os("XLFN_BENCH_REPORT_ONLY").is_some();
    for _ in 0..16 {
        drop(std::hint::black_box(construct(delivery, configured)));
    }
    for count in [1, 4, 32] {
        let mut sources = Vec::with_capacity(count);
        ALLOCS.set(0);
        DEALLOCS.set(0);
        REALLOCS.set(0);
        BYTES.set(0);
        ENABLED.set(true);
        for _ in 0..count {
            sources.push(construct(delivery, configured));
        }
        ENABLED.set(false);
        std::hint::black_box(&sources);
        let allocations = ALLOCS.get();
        let deallocations = DEALLOCS.get();
        let reallocations = REALLOCS.get();
        let requested_bytes = BYTES.get();

        for source in &sources {
            assert_eq!(source.limits(), limits(configured));
            assert_eq!(source.delivery(), delivery.delivery());
            assert_eq!(
                source.max_producers().get(),
                if configured { 7 } else { 64 }
            );
        }
        assert_eq!(FACTORY_CALLS.load(Ordering::Relaxed), 0);
        println!(
            "{}",
            serde_json::json!({
                "probe": "rtd_channel_source_construction",
                "case": if configured { "configured" } else { "default" },
                "sources": count,
                "caller_allocations": allocations,
                "caller_deallocations": deallocations,
                "caller_reallocations": reallocations,
                "caller_requested_bytes": requested_bytes,
                "factory_calls": FACTORY_CALLS.load(Ordering::Relaxed),
                "report_only": report_only,
            })
        );
        if !report_only {
            assert_eq!(allocations, count, "one shared allocation per source");
            assert_eq!(
                deallocations, 0,
                "source drops must remain outside measurement"
            );
            assert_eq!(reallocations, 0);
        }
        drop(sources);
    }
}

fn main() {
    run(RtdFifo::new(NonZeroUsize::new(8).unwrap()), false);
    run(RtdLatest, true);
}
