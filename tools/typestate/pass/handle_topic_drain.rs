// module: handle
fn ordered(table: &super::TopicTable) {
    let initializers = table.close();
    for initializer in initializers {
        initializer.notify_closed();
        initializer.wait_until_done();
    }
    drop(table.seal_and_drain());
}
