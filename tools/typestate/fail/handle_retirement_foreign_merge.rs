// module: handle/domain
use super::retirement::ReclaimBatch;

fn invalid(mut own: ReclaimBatch<'_>, foreign: ReclaimBatch<'_>) {
    own.extend(foreign); // error: E0599
}
