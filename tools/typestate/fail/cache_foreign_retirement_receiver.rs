// module: cache
use super::{CacheLookupDomain, ReclaimEntry};

fn invalid(domain: &CacheLookupDomain<u32>, entry: ReclaimEntry<u32>) {
    domain.enqueue_reclaim(entry); // error: E0599
}
