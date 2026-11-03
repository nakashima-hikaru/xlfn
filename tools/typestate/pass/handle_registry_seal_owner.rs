// module: handle
use super::store::HandleStore;

fn finish(store: &HandleStore) -> crate::XllResult<crate::shutdown::HandlesQuiescent> {
    let sealed = store.seal()?;
    sealed.finish_quiescence()?;
    sealed.quiescent(None)
}
