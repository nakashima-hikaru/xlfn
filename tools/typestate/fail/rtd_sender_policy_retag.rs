// module: subscription
use crate::rtd::{RtdLatest, RtdSender};

fn invalid(sender: RtdSender<i32, RtdLatest>) -> RtdSender<i32> {
    sender // error: E0308
}
