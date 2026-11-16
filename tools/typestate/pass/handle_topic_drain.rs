// module: handle
fn ordered(table: &super::TopicTable) {
    let initializers = table.close();
    initializers.notify_and_wait();
    drop(table.seal_and_drain());
}
