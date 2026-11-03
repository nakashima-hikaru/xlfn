// module: excel_rtd/windows/server
use super::{OwnedServerReference, RtdSubscription};
unsafe fn invalid<E: RtdSubscription, C: crate::RtdSourceCatalog<E>>(reference: OwnedServerReference) {
    let access = unsafe { reference.drain_access::<E, C>() }.unwrap();
    drop(reference); // error: E0505
    drop(access);
}
