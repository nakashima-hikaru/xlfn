use std::{num::NonZeroUsize, time::Duration};

use xlfn::{
    error::InputError,
    prelude::*,
    rtd::{RtdChannelSource, RtdProducerErrorPolicy, RtdSender, RtdValue},
};

use super::Client;

pub(crate) type MetricSource = RtdChannelSource<RtdValue>;

pub(crate) fn metric_source() -> MetricSource {
    RtdChannelSource::new(NonZeroUsize::new(64).unwrap(), |topic| {
        let mut parts = topic.parts();
        let (Some(kind), Some(symbol), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(XllError::input(
                "RTD topic",
                InputError::Malformed("expected [kind, symbol]"),
            ));
        };
        if kind != "last" {
            return Err(XllError::input(
                "RTD topic",
                InputError::Malformed("unsupported metric topic"),
            ));
        }
        let symbol = symbol.to_owned();
        // Each job owns its client; real integrations can open a connection here.
        let client = Client;
        Ok(move |sender: RtdSender<RtdValue>| {
            while !sender.is_closed() {
                match client.try_next_metric(&symbol) {
                    Ok(Some(value)) => sender
                        .try_send(RtdValue::Number(value))
                        .map_err(xlfn::rtd::RtdSendError::into_error)?,
                    Ok(None) => {
                        sender.wait_closed(Duration::from_millis(50));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(())
        })
    })
    .with_max_producers(NonZeroUsize::new(32).unwrap())
    .with_error_policy(RtdProducerErrorPolicy::PublishError(
        ExcelError::NotAvailable,
    ))
}
