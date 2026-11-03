// module: subscription/channel
use super::{Channel, RtdFifo, RtdLatest};
use triomphe::Arc;

fn retag(channel: &Arc<Channel<RtdFifo>>) {
    let _ = Channel::sender_with_policy::<i32>(channel, RtdLatest); // error: E0308
}
