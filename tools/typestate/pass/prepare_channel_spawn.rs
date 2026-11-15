// module: subscription/channel
use super::{PreparingLocalChannel, RtdChannelSubscription, RtdFifo, RtdProducerErrorPolicy, RtdSender, RtdTopic};

fn spawn(prepared: PreparingLocalChannel<RtdFifo>, delivery: RtdFifo, topic: RtdTopic) -> crate::XllResult<RtdChannelSubscription> {
    prepared.spawn(delivery, topic, |_: RtdSender<i32>| Ok(()), RtdProducerErrorPolicy::RetainLastValue)
}
