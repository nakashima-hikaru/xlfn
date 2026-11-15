// module: async_udf/builtin
use super::lifetime::PollerPrepared;

fn spawn_twice(poller: PollerPrepared<'_>) {
    let _running = poller.spawn();
    let _running_again = poller.spawn(); // error: E0382
}
