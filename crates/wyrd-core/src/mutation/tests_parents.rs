use super::*;

#[test]
fn parent_tokens_survive_descendants_and_rotate_after_replacement() {
    let queue = MutationQueue::default();
    let parent = queue.capture_parent("parent").unwrap().unwrap();
    let child = queue.capture_parent("parent/child").unwrap().unwrap();
    let sibling = queue.capture_parent("sibling").unwrap().unwrap();

    assert_eq!(queue.capture_parent("parent"), Ok(Some(parent)));
    assert_eq!(queue.capture_parent("parent/child"), Ok(Some(child)));
    assert_eq!(queue.capture_parent("sibling"), Ok(Some(sibling)));

    queue.invalidate_parent_subtree("parent");
    assert!(!queue.validate_parent("parent", parent).unwrap());
    assert!(!queue.validate_parent("parent/child", child).unwrap());
    assert!(queue.validate_parent("sibling", sibling).unwrap());
    assert_eq!(queue.capture_parent("parent"), Ok(None));

    queue.publish_parent_tokens();
    let replacement = queue.capture_parent("parent").unwrap().unwrap();
    assert_ne!(replacement, parent);
    assert_eq!(queue.capture_parent("sibling"), Ok(Some(sibling)));
}

#[test]
fn parent_tokens_block_all_until_a_new_projection_is_published() {
    let queue = MutationQueue::default();
    let root = queue.capture_parent("").unwrap().unwrap();
    let child = queue.capture_parent("child").unwrap().unwrap();

    queue.invalidate_parent_tokens();
    assert!(!queue.validate_parent("", root).unwrap());
    assert!(!queue.validate_parent("child", child).unwrap());
    assert_eq!(queue.capture_parent("child"), Ok(None));

    queue.publish_parent_tokens();
    assert_ne!(queue.capture_parent(""), Ok(Some(root)));
    assert_ne!(queue.capture_parent("child"), Ok(Some(child)));
}

#[test]
fn parent_tokens_fail_closed_at_the_configured_bound() {
    let queue = MutationQueue::with_limits(1, 1);
    let first = queue.capture_parent("first").unwrap().unwrap();
    assert_eq!(queue.capture_parent("first"), Ok(Some(first)));
    assert_eq!(queue.capture_parent("second"), Ok(None));

    queue.invalidate_parent_subtree("first");
    queue.publish_parent_tokens();
    assert!(queue.capture_parent("second").unwrap().is_some());
}

#[test]
fn parent_tokens_ignore_sibling_changes() {
    let queue = MutationQueue::default();
    let parent = queue.capture_parent("parent").unwrap().unwrap();
    let sibling = queue.capture_parent("sibling").unwrap().unwrap();

    queue.invalidate_parent_subtree("sibling");
    assert!(queue.validate_parent("parent", parent).unwrap());
    assert!(!queue.validate_parent("sibling", sibling).unwrap());
    queue.publish_parent_tokens();
    assert_eq!(queue.capture_parent("parent"), Ok(Some(parent)));
}
