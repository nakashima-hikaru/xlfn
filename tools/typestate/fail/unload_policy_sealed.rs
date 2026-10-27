// module: runtime
use super::UnloadPolicy;

struct UncheckedPhysicalUnload;

impl UnloadPolicy<()> for UncheckedPhysicalUnload { // error: E0277
    const PHYSICAL: bool = true;

    fn quiesce(_: &mut (), _: &mut ()) -> Result<(), crate::XllError> {
        Ok(())
    }
}
