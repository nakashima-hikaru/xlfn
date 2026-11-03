// module: runtime_components
fn invalid(services: &mut super::GenerationServices, generation: crate::generation::RuntimeGeneration) {
    services.generation = generation; // error: E0616
}
