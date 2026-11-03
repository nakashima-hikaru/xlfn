// module: subscription
use super::channel::AttachedChannel;

fn invalid(channel: AttachedChannel) {
    let _ = channel.permit; // error: E0616
}
