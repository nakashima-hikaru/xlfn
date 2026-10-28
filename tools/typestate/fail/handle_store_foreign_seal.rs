// module: handle
use super::store::HandleStore;

fn invalid(first: &HandleStore, second: &HandleStore) -> crate::XllResult<()> {
    let sealed = first.seal()?;
    let _ = second.quiescent(&sealed, None); // error: E0599
    Ok(())
}
