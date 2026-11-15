// module: async_udf/builtin
use super::lifetime::{PollerPool, Running};

fn shut_down_twice(pool: PollerPool<Running>) {
    let _joined = pool.shutdown();
    let _joined_again = pool.shutdown(); // error: E0382
}
