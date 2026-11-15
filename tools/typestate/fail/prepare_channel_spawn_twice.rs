// module: subscription/channel
use super::{PreparingLocalChannel, RtdFifo, RtdProducerErrorPolicy, RtdSender, RtdTopic};

fn invalid(prepared: PreparingLocalChannel<RtdFifo>, delivery: RtdFifo, topic: RtdTopic) {
    let _ = prepared.spawn(delivery, topic.clone(), |_: RtdSender<i32>| Ok(()), RtdProducerErrorPolicy::RetainLastValue);
    let _ = prepared.spawn(delivery, topic, |_: RtdSender<i32>| Ok(()), RtdProducerErrorPolicy::RetainLastValue); // error: E0382
}
