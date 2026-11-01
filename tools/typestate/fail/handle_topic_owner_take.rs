// module: handle
fn invalid(topic: super::topic::Topic) {
    drop(topic.publication); // error: E0616
}
