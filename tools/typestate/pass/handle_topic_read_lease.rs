// module: handle
use super::topic::{PublishedTopicState, TopicTable};
use super::HandleTopicKey;

fn read(table: &TopicTable, key: &HandleTopicKey) -> crate::XllResult<Option<PublishedTopicState>> {
    let lease = table.enter_read_lease()?;
    Ok(lease.load(key).map(|topic| topic.state()))
}
