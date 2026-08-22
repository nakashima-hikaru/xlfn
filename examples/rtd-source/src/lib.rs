#![deny(unsafe_op_in_unsafe_fn)]

use std::sync::Arc;

use xlfn::prelude::*;
use xlfn::rtd::{RtdTopic, RtdValue};

mod metric_source;
mod metric_subscription;

use metric_source::MetricSource;
pub(crate) use metric_subscription::MetricSubscription;

#[derive(Default)]
pub(crate) struct Client;

impl Client {
    fn try_next_metric(&self, _symbol: &str) -> Result<Option<f64>, std::io::Error> {
        Ok(None)
    }
}

pub struct State {
    metrics: RtdSourceHandle<MetricSource>,
}

#[excel_addin(name = "RTD Source Example", id = "rtd-source", category = "Examples")]
pub struct RtdSourceExample;

impl Addin for RtdSourceExample {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(
        context: &OpenContext,
    ) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State {
            metrics: context.rtd().register_source(MetricSource {
                client: Arc::new(Client),
            })?,
        }, (), ()))
    }
}

#[excel_function(name = "METRIC.LAST")]
pub fn last_metric(
    #[excel_context(main_thread)] context: MainThreadContext<'_, RtdSourceExample>,
    symbol: String,
) -> XllResult<RtdValue> {
    let topic = RtdTopic::new(["last", symbol.as_str()])?;
    context.subscribe(&context.state().metrics, topic)
}
