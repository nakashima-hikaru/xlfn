use xlfn::prelude::*;

struct State;

#[excel_addin(
    name = "Physical Unload",
    id = "physical-unload",
    category = "Test",
    physical_unload
)]
struct PhysicalUnloadAddin;

impl Addin for PhysicalUnloadAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(
        _: &OpenContext,
    ) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State, (), ()))
    }
}

// SAFETY: this compile fixture represents an add-in that synchronously stops
// every application-owned executable source before physical unload.
unsafe impl PhysicallyUnloadableAddin for PhysicalUnloadAddin {}

fn main() {}
