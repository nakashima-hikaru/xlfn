// module: handle/topic
fn invalid(table: &super::TopicTable, other: &super::TopicTable) {
    if let Some(Ok(batch)) = other.read_domain.poll_quiesce() {
        let _ = table.drain_generation(batch); // error: E0624
    }
}
