// module: handle
fn invalid(topic: &super::PublishedTopic) {
    topic.state.store(2, std::sync::atomic::Ordering::Release); // error: E0616
}
