// module: handle/domain
use std::sync::atomic::{AtomicUsize, Ordering};

fn invalid(counter: &AtomicUsize) {
    super::retirement::update_count(counter, 1, Ordering::Relaxed, super::counters::add); // error: E0603
}
