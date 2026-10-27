// module: handle
use super::object::PendingObjectBinding;
use super::registry::HandleRegistry;
use super::typed::{ExcelHandleObject, HandleAlias};

struct Payload;
impl ExcelHandleObject for Payload {}

fn allocate_and_publish(registry: &HandleRegistry) -> crate::XllResult<()> {
    let pending: PendingObjectBinding<'_, Payload> = registry.new_object(Payload)?;
    pending.publish(registry)?;
    Ok(())
}

fn alias_and_publish<'registry>(
    registry: &'registry HandleRegistry,
    alias: HandleAlias<'_, Payload>,
) -> crate::XllResult<()> {
    let pending = PendingObjectBinding::from_alias(registry, alias)?;
    pending.publish(registry)?;
    Ok(())
}
