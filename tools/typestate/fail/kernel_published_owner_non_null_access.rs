// module: kernel/lib
use crate::published_owner::PublishedOwner;

fn read_without_proving_access(owner: &PublishedOwner<u32>) -> &u32 {
    owner.as_non_null().as_ref() // error: E0133
}
