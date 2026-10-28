//! A consumer with no RTD declarations, even when its dependency enables RTD.
use framework::prelude::*;

pub struct BasicAddin;

#[excel_addin(name = "RTD API consumer", id = "rtd-api-consumer")]
impl Addin for BasicAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(()))
    }
}

#[excel_function(name = "CONSUMER.ADD", thread_safe)]
pub fn add(left: f64, right: f64) -> f64 {
    left + right
}
