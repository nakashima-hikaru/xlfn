// module: handle
use super::registry::HandleRegistry;

fn invalid(first: &HandleRegistry, second: &HandleRegistry) -> crate::XllResult<()> {
    let sealed = first.seal()?;
    second.finish_quiescence(&sealed) // error: E0599
}
