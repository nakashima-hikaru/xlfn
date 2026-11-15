// module: handle/registry
use super::{HandleRegistry, ObjectBinding};

fn ordered(registry: &HandleRegistry, object: ObjectBinding) -> crate::XllResult<()> {
    let validated = registry.validate_binding(object)?;
    let reserved = validated.reserve()?;
    let published = reserved.publish()?;
    let _id = published.id;
    Ok(())
}
