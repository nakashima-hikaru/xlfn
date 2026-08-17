use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use xlfn::{
    error::InputError,
    prelude::*,
    rtd::{RtdSink, RtdSource, RtdSubscription, RtdTopic, RtdValue},
};

use super::{Client, MetricSubscription};

pub(crate) struct MetricSource {
    pub(crate) client: Arc<Client>,
}

impl RtdSource for MetricSource {
    type Value = RtdValue;

    fn subscribe(
        &self,
        topic: &RtdTopic,
        sink: RtdSink<Self::Value>,
    ) -> XllResult<Box<dyn RtdSubscription>> {
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
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let client = Arc::clone(&self.client);

        let worker = std::thread::spawn(move || {
            while !worker_cancelled.load(Ordering::Relaxed) {
                match client.try_next_metric(&symbol) {
                    Ok(Some(value)) => {
                        if sink.publish(RtdValue::Number(value)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => {
                        let _ = sink
                            .publish(RtdValue::Error(ExcelErrorValue(ExcelError::NotAvailable)));
                        break;
                    }
                }
            }
        });

        Ok(Box::new(MetricSubscription {
            cancelled,
            worker: Some(worker),
        }))
    }
}
