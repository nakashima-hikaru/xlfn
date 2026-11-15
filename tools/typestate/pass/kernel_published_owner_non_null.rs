// module: kernel/lib
use crate::published_owner::PublishedOwner;
use std::ptr::NonNull;

fn observe_unsized<T: ?Sized>(owner: &PublishedOwner<T>) -> NonNull<T> {
    owner.as_non_null()
}

fn recover(owner: PublishedOwner<[u32]>) -> Box<[u32]> {
    let _ = observe_unsized(&owner);
    owner.into_box()
}
