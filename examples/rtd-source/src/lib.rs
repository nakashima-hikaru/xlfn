#![deny(unsafe_op_in_unsafe_fn)]

use xlfn::prelude::*;
use xlfn::rtd::{RtdSourceHandle, RtdValue};

#[allow(
    dead_code,
    reason = "Compile the independent blocking-producer alternative"
)]
mod metric_source;
mod multiplexed_metric_source;

use multiplexed_metric_source::{MetricSource, metric_source};

#[derive(Default)]
pub(crate) struct Client;

impl Client {
    // These placeholders stand for one bounded, cancellation-aware connection.
    fn subscribe(&mut self, _symbol: &str) -> Result<(), std::io::Error> {
        Ok(())
    }

    fn unsubscribe(&mut self, _symbol: &str) -> Result<(), std::io::Error> {
        Ok(())
    }

    fn try_next_update(&mut self) -> Result<Option<(String, f64)>, std::io::Error> {
        Ok(None)
    }

    #[allow(dead_code, reason = "Used by the blocking-producer alternative")]
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
    type RtdSubscription = <MetricSource as xlfn::rtd::RtdSource>::Subscription;
    type RtdSources = (MetricSource,);
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;

    fn open(
        context: &OpenContext<Self>,
    ) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        let (metrics,) = context.rtd().register_sources((metric_source(),))?;
        Ok(Opened::new(State { metrics }))
    }
}

#[excel_function(name = "METRIC.LAST")]
pub fn last_metric(
    context: MainThreadContext<'_, RtdSourceExample>,
    symbol: String,
) -> XllResult<RtdValue> {
    context
        .rtd()
        .subscribe(&context.state().metrics, &["last", symbol.as_str()])
}
