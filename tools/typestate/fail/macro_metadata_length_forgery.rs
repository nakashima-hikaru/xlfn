// module: macros/lib
fn forge() -> super::validation::ValidatedAddinMetadata {
    super::validation::ValidatedAddinMetadata { name_units: vec![65], name_length: 2 } // error: E0451
}
