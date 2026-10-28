use framework::prelude::*;

pub struct ConfiguredAddin;

#[excel_addin(name = "Configured RTD consumer", id = "configured-rtd-consumer")]
impl Addin for ConfiguredAddin {
    type RtdSubscription = ();
    type RtdSources = ();
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_context: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(()))
    }
}
