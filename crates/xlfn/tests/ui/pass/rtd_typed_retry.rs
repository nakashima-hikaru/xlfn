use std::rc::Rc;
use xlfn::rtd::{IntoRtdValue, RtdPendingValue, RtdSendError, RtdSender, RtdValue};
use xlfn::XllResult;

// Deliberately neither Debug nor Clone nor Send: conversion consumes the input.
struct Input(Rc<()>);
impl IntoRtdValue for Input {
    fn into_rtd_value(self) -> XllResult<RtdValue> {
        Ok(RtdValue::Integer(Rc::strong_count(&self.0) as i32))
    }
}
fn retry(sender: &RtdSender<Input>, error: RtdSendError<Input>) -> Result<(), RtdSendError<Input>> {
    let _ = format!("{error:?}");
    match error.into_pending() {
        Ok(value) => sender.try_send_pending(value),
        Err(error) => Err(RtdSendError::Invalid(error)),
    }
}
fn assert_payload_traits<T: Send + Sync + std::fmt::Debug>() {}
fn assert_error<T: std::error::Error>() {}
fn main() {
    assert_payload_traits::<RtdPendingValue<Input>>();
    assert_payload_traits::<RtdSendError<Input>>();
    assert_error::<RtdSendError<Input>>();
}
