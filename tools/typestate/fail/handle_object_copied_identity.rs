// module: handle
use super::object::ObjectArena;
use super::token::ObjectId;
use xlfn_kernel::published_owner::PublishedOwner;

fn invalid(arena: &PublishedOwner<ObjectArena>, id: ObjectId) {
    let _ = ObjectArena::insert(arena, id, 42_u32); // error: E0599
}
