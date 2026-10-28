// module: subscription
use super::channel::AttachedChannel;

fn invalid(channel: AttachedChannel) {
    let _ = channel.into_multiplexed_parts();
    let _ = channel.into_multiplexed_parts(); // error: E0382
}
