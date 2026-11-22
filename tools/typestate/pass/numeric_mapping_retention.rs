// module: host/process_quota/windows
use super::Mapping;

fn retain(mapping: Mapping) {
    let state = mapping.initialize_and_retain();
    if let Ok(state) = state {
        let _ = state.validate();
    }
}
