// module: cache
use super::LeaderGuard;

fn complete_twice(guard: LeaderGuard<'_, u64, u64>) {
    guard.complete(Err(Box::new(super::XllError::OVERLOADED)));
    guard.complete(Err(Box::new(super::XllError::OVERLOADED))); // error: E0382
}
