use framework::prelude::*;

pub struct BasicAddin;

#[excel_addin]
impl Addin for BasicAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(context: &OpenContext<Self>) -> OpenResult<Self> {
        let _ = context.rtd();
        Ok(Opened::new(()))
    }
}
