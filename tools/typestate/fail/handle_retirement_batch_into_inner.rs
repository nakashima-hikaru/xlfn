// module: handle/domain
use super::retirement::ReclaimBatch;

fn invalid(batch: ReclaimBatch<'_>) {
    let _ = batch.into_inner(); // error: E0599
}
