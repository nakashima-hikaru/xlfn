#![cfg(feature = "rtd")]

use xlfn::rtd::{RtdTopic, RtdTopicParts};

#[test]
fn topic_parts_borrow_text_and_report_remaining_length() {
    let topic = RtdTopic::new([String::from("market"), String::from("USD\0JPY")]).unwrap();
    assert_eq!(topic.len(), 2);
    assert!(!topic.is_empty());
    assert_eq!(topic.part(0), Some("market"));
    assert_eq!(topic.part(1), Some("USD\0JPY"));
    assert_eq!(topic.part(2), None);
    assert_eq!(topic.part(usize::MAX), None);

    let mut parts: RtdTopicParts<'_> = topic.parts();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts.size_hint(), (2, Some(2)));
    let first = parts.next().unwrap();
    assert_eq!(first.as_ptr(), topic.part(0).unwrap().as_ptr());
    assert_eq!(parts.len(), 1);
    assert_eq!(parts.next(), Some("USD\0JPY"));
    assert_eq!(parts.len(), 0);
    assert_eq!(parts.size_hint(), (0, Some(0)));
    assert_eq!(parts.next(), None);
    assert_eq!(parts.next(), None);
}

#[test]
fn cloning_topics_preserves_shared_long_text() {
    let input = "long-topic-part".repeat(16);
    let topic = RtdTopic::single(&input).unwrap();
    let cloned = topic.clone();
    assert_eq!(topic, cloned);
    assert_eq!(topic.part(0), Some(input.as_str()));
    assert_eq!(
        topic.part(0).unwrap().as_ptr(),
        cloned.part(0).unwrap().as_ptr()
    );
    assert!(topic.parts().eq(cloned.parts()));
}
