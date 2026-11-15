// module: async_udf/builtin
use super::lifetime::{PollerPool, Running};

fn finish_early(pool: PollerPool<Running>) {
    let _outcome = pool.finish(); // error: E0599
}
