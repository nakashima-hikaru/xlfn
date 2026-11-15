// module: async_udf/builtin
use super::lifetime::RunningPoller;

fn join_twice(poller: RunningPoller) {
    let _joined = poller.join();
    let _joined_again = poller.join(); // error: E0382
}
