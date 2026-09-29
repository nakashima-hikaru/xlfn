use xlfn::rtd::{RtdPendingValue, RtdSender};

fn retry(sender: &RtdSender<String>, value: RtdPendingValue<f64>) {
    let _ = sender.try_send_pending(value);
}

fn main() {}
