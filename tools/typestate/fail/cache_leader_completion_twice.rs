// module: cache
use super::LeaderGuard;

fn complete_twice(guard: LeaderGuard<'_, u64, u64>) {
    guard.complete(Err(Box::new(super::XllError::Overloaded)));
    guard.complete(Err(Box::new(super::XllError::Overloaded))); // error: E0382
}
