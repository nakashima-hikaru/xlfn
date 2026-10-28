// module: cache
use super::LeaderGuard;

fn unregister_early(mut guard: LeaderGuard<'_, u64, u64>) {
    guard.unregister(); // error: E0599
}
