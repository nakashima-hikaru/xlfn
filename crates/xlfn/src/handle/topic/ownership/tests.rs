use super::*;
use crate::handle::formula::test_topic_key;

#[test]
#[allow(
    clippy::drop_non_drop,
    clippy::forget_non_drop,
    reason = "protected publication must retain allocation even when field drop is the only cleanup or Drop is skipped"
)]
fn miri_published_topic_abandonment_keeps_live_reader_allocation() {
    for abandonment in [0, 1, 2] {
        let table = TopicTable::new();
        let key = test_topic_key("protected-topic-abandonment");
        let owner = PublishedTopicOwner::new(PublishedTopic::new(
            "token".to_owned(),
            "lifetime".to_owned(),
        ));
        let pointer = owner.owner.as_ptr();
        table
            .published
            .entries
            .pin()
            .insert(key, PublishedTopicPtr::from_ref(owner.as_ref()));
        let reader = table.enter_read_lease().unwrap();
        let publication = reader.load(&key).unwrap();
        match abandonment {
            0 => drop(owner),
            1 => std::mem::forget(owner),
            _ => {
                let outcome = crate::panic_boundary::catch_no_unwind(|| {
                    let _owner = owner;
                    panic!("abandon published topic during unwind");
                });
                assert!(outcome.is_err());
            }
        }
        // This is a real read-domain-protected observation after ownership
        // abandonment, rather than a test of wrapper size or move semantics.
        assert_eq!(publication.token, "token");
        assert_eq!(publication.lifetime_key, "lifetime");
        table.published.remove(key);
        drop(reader);
        drop(table.seal_and_drain());
        // SAFETY: private fault injection intentionally lost the protected
        // owner. Its publication is withdrawn, the actual domain is sealed
        // and reader-drained, and this test retained the sole recovery pointer.
        unsafe { drop(Box::from_raw(pointer)) };
    }
}

#[test]
fn drained_topic_owner_reclaims_only_after_reader_release() {
    let table = TopicTable::new();
    let owner = PublishedTopicOwner::new(PublishedTopic::new(
        "retired".to_owned(),
        "lifetime".to_owned(),
    ));
    let key = test_topic_key("retired-topic-reader");
    table
        .published
        .entries
        .pin()
        .insert(key, PublishedTopicPtr::from_ref(owner.as_ref()));
    let reader = table.enter_read_lease().unwrap();
    let publication = reader.load(&key).unwrap();
    table.published.remove(key);
    table.enqueue_reclaim(owner);
    assert_eq!(table.try_quiesce_and_drain().len(), 0);
    assert_eq!(publication.token, "retired");
    drop(reader);
    assert_eq!(table.queued.load(Ordering::Relaxed), 0);
    assert_eq!(table.seal_and_drain().len(), 0);
}
