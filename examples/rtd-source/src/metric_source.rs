use std::{num::NonZeroUsize, time::Duration};

use xlfn::{error::InputError, prelude::*, rtd::RtdValue};

use super::Client;

pub(crate) type MetricSource = RtdChannelSource<RtdValue>;

pub(crate) fn metric_source() -> MetricSource {
    RtdChannelSource::new(NonZeroUsize::new(64).unwrap(), |topic| {
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
        let symbol = symbol.clone();
        // Each job owns its client; real integrations can open a connection here.
        let client = Client;
        Ok(move |sender: RtdSender<RtdValue>| {
            while !sender.is_closed() {
                match client.try_next_metric(&symbol) {
                    Ok(Some(value)) => sender.try_send(RtdValue::Number(value))?,
                    Ok(None) => {
                        sender.wait_closed(Duration::from_millis(50));
                    }
                    Err(_) => {
                        sender
                            .try_send(RtdValue::Error(ExcelErrorValue(ExcelError::NotAvailable)))?;
                        break;
                    }
                }
            }
            Ok(())
        })
    })
}
