use framework::prelude::*;
use framework::rtd::{
    BoxedRtdSubscription, RtdChannelSource, RtdLatest, RtdMultiplexedSource,
    RtdSender, RtdSourceContext, RtdSourceHandle, RtdValue,
};
use std::num::NonZeroUsize;

pub struct State {
    blocking: RtdSourceHandle,
    shared: RtdSourceHandle,
}

pub struct ErasedAddin;

#[excel_addin(name = "Erased RTD consumer", id = "erased-rtd-consumer")]
impl Addin for ErasedAddin {
    type SharedState = State;
    type RtdSubscription = BoxedRtdSubscription;

    fn open(context: &OpenContext<Self>) -> OpenResult<Self> {
        let initial = 42;
        let blocking = context.rtd().register(RtdChannelSource::new(
            NonZeroUsize::new(4).unwrap(),
            move |_topic| Ok(move |_sender: RtdSender<i32>| {
                let _captured = initial;
                Ok(())
            }),
        ))?;
        let label = "captured source".to_owned();
        let shared = context.rtd().register(RtdMultiplexedSource::new(
            RtdLatest,
            move |_context: RtdSourceContext<String, RtdLatest>| {
                drop(label);
                Ok(())
            },
        ))?;
        Ok(Opened::new(State { blocking, shared }))
    }
}

#[excel_function(name = "CONSUMER.ERASED")]
fn value(context: MainThreadContext<'_, ErasedAddin>, shared: bool) -> XllResult<RtdValue> {
    let state = context.state();
    let handle = if shared { &state.shared } else { &state.blocking };
    context.rtd().subscribe(handle, &["value"])
}
