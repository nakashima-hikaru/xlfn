// module: cache
use super::reclamation::ReclaimEntries;

fn invalid(mut own: ReclaimEntries<'_, u32>, foreign: ReclaimEntries<'_, u32>) {
    own.extend(foreign); // error: E0599
}
