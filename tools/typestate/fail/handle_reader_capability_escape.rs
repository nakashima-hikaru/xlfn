// module: handle
use super::topic::{TopicReadLease, TopicTable};
use super::{HandleDomainPermit, HandleTopicKey};

fn cannot_drop_lease_before_topic(table: &TopicTable, key: &HandleTopicKey) {
    let lease = table.enter_read_lease().unwrap();
    let topic = lease.load(key).unwrap();
    drop(lease); // error: E0505
    let _ = topic.state();
}

fn maintenance_retains_table_until_implicit_drop() {
    let _lease;
    {
        let table = TopicTable::new();
        _lease = table.enter_read_lease().unwrap(); // error: E0597
    }
}

fn cannot_split_fields(topic: TopicReadLease<'_>, handle: HandleDomainPermit) {
    drop(topic.maintenance); // error: E0616
    drop(handle.maintenance); // error: E0616
}
