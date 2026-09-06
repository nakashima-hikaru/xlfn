use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use xlfn::{error::InputError, prelude::*, rtd::RtdValue};

use super::Client;

pub(crate) type MetricSource = RtdChannelSource<RtdValue>;

pub(crate) fn metric_source(client: Arc<Client>) -> MetricSource {
    RtdChannelSource::new(NonZeroUsize::new(64).unwrap(), move |topic, sender| {
        let [kind, symbol] = topic.parts() else {
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
        while !sender.is_closed() {
            match client.try_next_metric(symbol) {
                Ok(Some(value)) => sender.try_send(RtdValue::Number(value))?,
                Ok(None) => {
                    sender.wait_closed(Duration::from_millis(50));
                }
                Err(_) => {
                    sender.try_send(RtdValue::Error(ExcelErrorValue(ExcelError::NotAvailable)))?;
                    break;
                }
            }
        }
        Ok(())
    })
}
