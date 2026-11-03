use std::{collections::HashMap, time::Duration};

use xlfn::{
    error::InputError,
    prelude::*,
    rtd::{
        RtdLatest, RtdMultiplexedSource, RtdProducerErrorPolicy, RtdSendError, RtdSender,
        RtdSource, RtdSourceContext, RtdSourceEvent, RtdSubscriptionId, RtdTopic, RtdValue,
    },
};

use super::Client;

pub(crate) fn metric_source() -> impl RtdSource<Value = RtdValue> {
    RtdMultiplexedSource::new(RtdLatest, produce_metrics)
        .with_topic_validator(validate_topic)
        .with_error_policy(RtdProducerErrorPolicy::PublishError(
            ExcelError::NotAvailable,
        ))
}

fn produce_metrics(context: RtdSourceContext<RtdValue, RtdLatest>) -> XllResult<()> {
    // One job owns the shared connection and all of its topic routing.
    let mut client = Client;
    let mut topics: HashMap<String, HashMap<RtdSubscriptionId, RtdSender<RtdValue, RtdLatest>>> =
        HashMap::new();
    let mut symbols = HashMap::new();

    while !context.is_closed() {
        // Bound each turn so subscription churn cannot starve incoming data.
        for turn in 0..64 {
            let timeout = if turn == 0 {
                Duration::from_millis(20)
            } else {
                Duration::ZERO
            };
            let event = match context.recv_timeout(timeout) {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(XllError::Closing) if context.is_closed() => return Ok(()),
                Err(error) => return Err(error),
            };
            match event {
                RtdSourceEvent::Subscribe { id, topic, sender } => {
                    let symbol = topic.part(1).expect("validated symbol").to_owned();
                    if !topics.contains_key(&symbol) {
                        client.subscribe(&symbol)?;
                    }
                    topics.entry(symbol.clone()).or_default().insert(id, sender);
                    symbols.insert(id, symbol);
                }
                RtdSourceEvent::Unsubscribe { id } => {
                    if let Some(symbol) = symbols.remove(&id)
                        && let Some(subscribers) = topics.get_mut(&symbol)
                    {
                        subscribers.remove(&id);
                        if subscribers.is_empty() {
                            topics.remove(&symbol);
                            client.unsubscribe(&symbol)?;
                        }
                    }
                }
                _ => {}
            }
        }

        // The connection yields updates by symbol; no scan over idle topics.
        for _ in 0..64 {
            if context.is_closed() {
                return Ok(());
            }
            let Some((symbol, value)) = client.try_next_update()? else {
                break;
            };
            if let Some(subscribers) = topics.get(&symbol) {
                for sender in subscribers.values() {
                    match sender.try_send(RtdValue::Number(value)) {
                        Ok(()) | Err(RtdSendError::Closed(_)) => {}
                        // This example samples state: discard an update if
                        // the shared value/byte budget is exhausted.
                        Err(RtdSendError::Full(_)) => {}
                        Err(RtdSendError::Invalid(error)) => return Err(error),
                        Err(error) => return Err(error.into_error()),
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_topic(topic: &RtdTopic) -> XllResult<()> {
    if topic.len() != 2 || topic.part(0) != Some("last") {
        return Err(XllError::input(
            "RTD topic",
            InputError::Malformed("expected [last, symbol]"),
        ));
    }
    Ok(())
}
