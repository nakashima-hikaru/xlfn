// module: macros/lib
fn forge(ty: syn::Type) -> super::model::ValidatedContext {
    super::model::ValidatedContext { ty: ty.clone(), addin: ty } // error: E0451
}
