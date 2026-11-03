// module: subscription/channel
use super::{Channel, RtdFifo, RtdLatest, RtdSender};
use triomphe::Arc;

fn fifo(channel: &Arc<Channel<RtdFifo>>, policy: RtdFifo) -> RtdSender<i32, RtdFifo> {
    Channel::sender_with_policy(channel, policy)
}
fn latest(channel: &Arc<Channel<RtdLatest>>) -> RtdSender<i32, RtdLatest> {
    Channel::sender_with_policy(channel, RtdLatest)
}
