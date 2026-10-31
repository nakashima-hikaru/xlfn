// module: handle/domain
use super::retirement::ReclaimBatch;

fn invalid(batch: ReclaimBatch<'_>) {
    batch.reclaim();
    batch.reclaim(); // error: E0382
}
