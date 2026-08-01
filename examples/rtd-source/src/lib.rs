#![deny(unsafe_op_in_unsafe_fn)]

use std::sync::Arc;

use xlfn::prelude::*;

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
    metrics: Arc<MetricSource>,
}

#[excel_addin(name = "RTD Source Example", id = "rtd-source", category = "Examples")]
pub struct RtdSourceExample;

impl Addin for RtdSourceExample {
    type State = State;
    type Error = XllError;

    fn open(_context: &OpenContext) -> Result<Self::State, Self::Error> {
        Ok(State {
            metrics: Arc::new(MetricSource {
                client: Arc::new(Client),
            }),
        })
    }
}

#[excel_function(name = "METRIC.LAST")]
pub fn last_metric(
    #[excel_context(main_thread)] context: MainThreadContext<'_, State>,
    symbol: String,
) -> XllResult<RtdValue> {
    let topic = RtdTopic::new(["last", symbol.as_str()])?;
    context.subscribe(Arc::clone(&context.state().metrics), topic)
}
