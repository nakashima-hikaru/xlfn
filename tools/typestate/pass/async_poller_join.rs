// module: async_udf/builtin
use super::lifetime::{PollerPool, PollerPrepared, Running};

fn spawn_and_join(poller: PollerPrepared<'_>) -> crate::XllResult<()> {
    let _joined = poller.spawn()?.join();
    Ok(())
}

fn shutdown(pool: PollerPool<Running>) -> crate::XllResult<()> {
    pool.shutdown().finish()
}
