// module: cache
fn invalid<V>(node: super::NodePtr<V>) {
    super::resident_index::ResidentEntry::new(node, 1); // error: E0308
}
