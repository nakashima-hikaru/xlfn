// module: subscription
use super::channel::RtdChannelSubscription;

fn invalid(channel: RtdChannelSubscription) {
    let _ = channel.into_multiplexed_parts(); // error: E0599
}
