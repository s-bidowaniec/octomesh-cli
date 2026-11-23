use octo_core::Blackboard;

#[test]
fn inbox_acknowledges_only_the_returned_page_for_each_peer() {
    let board = Blackboard::open_in_memory().unwrap();
    let direct_ids = board
        .send_messages(
            "vibe",
            &["agy".into(), "agy".into()],
            "review",
            Some("task-1"),
        )
        .unwrap();
    assert_eq!(direct_ids.len(), 1);
    let broadcast_ids = board
        .send_messages("vibe", &["*".into()], "broadcast hello", None)
        .unwrap();

    let page = board.read_inbox("agy", true, 1, true).unwrap();
    assert_eq!(page.marked_read, 1);
    assert_eq!(page.unread, 1);
    assert_eq!(page.messages[0].id, broadcast_ids[0]);
    assert!(page.messages[0].read_at.is_some());

    let page = board.read_inbox("vibe", true, 20, false).unwrap();
    assert_eq!(page.unread, 1);
    assert_eq!(page.messages[0].id, broadcast_ids[0]);
    assert!(page.messages[0].read_at.is_none());

    let page = board.read_inbox("agy", true, 0, true).unwrap();
    assert_eq!(page.unread, 1);
    assert_eq!(page.marked_read, 0);
    assert!(page.messages.is_empty());

    let page = board.read_inbox("agy", true, 1, true).unwrap();
    assert_eq!(page.marked_read, 1);
    assert_eq!(page.unread, 0);
    assert_eq!(page.messages[0].id, direct_ids[0]);
    assert_eq!(page.messages[0].sender, "vibe");
    assert_eq!(page.messages[0].ref_task.as_deref(), Some("task-1"));

    let page = board.read_inbox("agy", false, 20, true).unwrap();
    assert_eq!(page.marked_read, 0);
    assert_eq!(page.messages.len(), 2);
    assert!(page
        .messages
        .iter()
        .all(|message| message.read_at.is_some()));
}
