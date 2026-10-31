//! Ownership recovery across user callback and error-clone panics.
use super::{GenerationServiceSlot, GenerationServiceState, ServiceFault, ServiceSlotError};
use std::mem::ManuallyDrop;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn miri_initialization_callback_panic_drops_unpublished_candidate_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let slot = Arc::new(GenerationServiceSlot::<(), DropProbe, ()>::new());
    slot.arm(()).unwrap();
    let initializing = Arc::clone(&slot);
    let candidate_drops = Arc::clone(&drops);
    let initializer = std::thread::spawn(move || {
        let _result = initializing.read(
            |_| Ok(Box::new(DropProbe(candidate_drops))),
            |_| panic!("injected initialization callback panic"),
        );
    });
    assert!(initializer.join().is_err());
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(slot.published.load(Ordering::Acquire).is_null());
    assert!(slot.readers.is_sealed());
    assert!(matches!(
        slot.read(
            |_| panic!("faulted service must not initialize again"),
            |_| {}
        ),
        Err(ServiceSlotError::Fault(ServiceFault::Panicked))
    ));
    slot.disarm().unwrap();
    drop(slot);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[derive(Debug)]
struct ClonePanics;

impl Clone for ClonePanics {
    fn clone(&self) -> Self {
        panic!("injected shutdown error clone panic")
    }
}

#[test]
fn miri_shutdown_error_clone_panic_quarantines_original_allocation() {
    let drops = Arc::new(AtomicUsize::new(0));
    let slot = Arc::new(GenerationServiceSlot::<(), DropProbe, ClonePanics>::new());
    slot.arm(()).unwrap();
    let read = slot
        .read(|_| Ok(Box::new(DropProbe(Arc::clone(&drops)))), |_| {})
        .unwrap();
    let original_address = std::ptr::from_ref(&*read).addr();
    drop(read);
    let sealing = Arc::clone(&slot);
    let sealer = std::thread::spawn(move || {
        let _result = sealing.seal(|| (), |_| Err::<(), _>(ClonePanics));
    });
    assert!(sealer.join().is_err());
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert!(slot.published.load(Ordering::Acquire).is_null());
    assert!(slot.read_if_ready().is_none());
    assert!(matches!(
        slot.seal(|| (), |_| Ok(())),
        Err(ServiceSlotError::Fault(ServiceFault::Panicked))
    ));

    // The public protocol retains this allocation permanently. The test has
    // joined the only other thread and ended every reader, so exclusive access
    // can inspect and dispose the retained owner without leaking test memory.
    let slot = Arc::try_unwrap(slot)
        .ok()
        .expect("all worker references have been released");
    let GenerationServiceState::TeardownFaulted { fault, runtime } = slot.state.into_inner() else {
        panic!("shutdown clone panic must retain a teardown fault");
    };
    assert!(matches!(fault.as_ref(), ServiceFault::Panicked));
    let runtime = ManuallyDrop::into_inner(runtime);
    assert_eq!(runtime.as_ptr().addr(), original_address);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    drop(runtime);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}
