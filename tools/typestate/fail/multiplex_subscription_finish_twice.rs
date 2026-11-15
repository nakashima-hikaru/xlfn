// module: subscription/multiplex
use super::RtdMultiplexedSubscription;

fn finish_twice(subscription: RtdMultiplexedSubscription<i32>) {
    let _ = subscription.finish();
    let _ = subscription.finish(); // error: E0382
}
