// module: handle/domain
use super::retirement::ReclaimBatch;

fn invalid(batch: ReclaimBatch<'_>) {
    let _ = batch.records; // error: E0616
}
