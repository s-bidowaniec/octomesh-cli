use octo_core::Blackboard;

#[test]
fn test_mailbox_send_list_mark_read() {
    let board = Blackboard::open_in_memory().unwrap();

    let m1 = board
        .send_message("vibe", "agy", "please review PR #12", Some("task-1"))
        .unwrap();
    board.send_message("codex", "agy", "LGTM", None).unwrap();
    board
        .send_message("vibe", "all", "broadcast hello", None)
        .unwrap();

    let unread = board.list_messages("agy", true, 20).unwrap();
    assert_eq!(unread.len(), 3);
    assert!(unread.iter().all(|m| m.read_at.is_none()));

    let vibe_msgs = board.list_messages("vibe", true, 20).unwrap();
    assert_eq!(vibe_msgs.len(), 1);
    assert_eq!(vibe_msgs[0].recipient, "all");

    assert_eq!(board.unread_count("agy").unwrap(), 3);
    assert_eq!(board.unread_count("vibe").unwrap(), 1);

    let marked = board.mark_messages_read("agy").unwrap();
    assert_eq!(marked, 3);
    assert_eq!(board.unread_count("agy").unwrap(), 0);

    let all_msgs = board.list_messages("agy", false, 20).unwrap();
    assert_eq!(all_msgs.len(), 3);
    assert!(all_msgs.iter().all(|m| m.read_at.is_some()));

    assert_eq!(m1.sender, "vibe");
    assert_eq!(m1.ref_task.as_deref(), Some("task-1"));
}
