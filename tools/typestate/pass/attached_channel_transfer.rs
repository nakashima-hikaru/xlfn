// module: subscription
use super::channel::AttachedChannel;

fn transfer(channel: AttachedChannel) {
    let (channel, permit) = channel.into_multiplexed_parts();
    drop(channel);
    drop(permit);
}
