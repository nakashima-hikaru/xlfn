// module: return_abi/numeric/process/windows
use super::Mapping;

fn invalid(mapping: Mapping) {
    let _ = mapping.initialize_and_retain();
    let _ = mapping.initialize_and_retain(); // error: E0382
}
