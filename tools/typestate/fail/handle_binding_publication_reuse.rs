// module: handle/registry
use super::{PublishedBinding, ReservedBinding, ValidatedBinding};

fn reserve_twice(binding: ValidatedBinding<'_>) {
    let _ = binding.reserve();
    let _ = binding.reserve(); // error: E0382
}

fn publish_twice(binding: ReservedBinding<'_>) {
    let _ = binding.publish();
    let _ = binding.publish(); // error: E0382
}

fn recover_after_publish(binding: PublishedBinding) {
    let _ = binding.object; // error: E0609
}
