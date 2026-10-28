// module: cache
use super::LeaderGuard;

fn complete(guard: LeaderGuard<'_, u64, u64>) {
    guard.complete(Err(Box::new(crate::XllError::Overloaded)));
}
