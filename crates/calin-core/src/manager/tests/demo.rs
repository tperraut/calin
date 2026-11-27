//! The mock agent's demo directives under the real manager. The tool
//! endpoint itself is served by `calin-server`, whose tests run the whole
//! orchestration; here nothing answers on it, which shows what the
//! orchestrator does with the calls themselves.

use super::*;

/// A loopback URL where nothing listens.
fn nobody_listening() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}/api/v1/mcp")
}

fn texts(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::ItemCompleted { item } => match &item.content {
                ItemContent::Text { text } => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn tool_calls(events: &[Event]) -> Vec<(String, ItemStatus)> {
    events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::ItemCompleted { item } => match &item.content {
                ItemContent::Tool { name, .. } => Some((name.clone(), item.status)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn without_a_tool_endpoint_a_directive_says_so_and_the_session_rests_in_review() {
    let dir = TempDir::new().unwrap();
    let (manager, _starts) = manager_in(&dir, true).await;
    for prompt in ["Ship it demo: orchestrate", "Which one? demo: ask"] {
        let id = manager
            .create_session(request(prompt, None))
            .await
            .unwrap()
            .session
            .id;
        let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
        let [text] = texts(&events).try_into().expect("one text");
        assert!(
            text.starts_with("Calin's tool endpoint is unavailable (none was given)"),
            "{text}"
        );
        assert!(tool_calls(&events).is_empty());
        assert!(!events.iter().any(is_status(SessionStatus::Failed)));
    }
}

#[tokio::test]
async fn in_ask_mode_the_policy_allows_calins_own_tools_and_a_failed_call_does_not_end_the_turn() {
    let dir = TempDir::new().unwrap();
    let (manager, _starts) = manager_in(&dir, true).await;
    manager.set_tool_endpoint(nobody_listening());
    let id = manager
        .create_session(request_in(
            PermissionMode::Ask,
            "Ship it demo: orchestrate",
            None,
        ))
        .await
        .unwrap()
        .session
        .id;
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    let requested: Vec<(String, String)> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::PermissionRequested { request } => {
                Some((request.id.clone(), request.tool_name.clone()))
            }
            _ => None,
        })
        .collect();
    let names: Vec<&str> = requested.iter().map(|(_, name)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "mcp__calin__dispatch",
            "mcp__calin__dispatch",
            "mcp__calin__send_message"
        ]
    );
    let resolved: Vec<(String, PermissionDecision, DecisionSource)> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::PermissionResolved {
                request_id,
                decision,
                by,
                ..
            } => Some((request_id.clone(), *decision, *by)),
            _ => None,
        })
        .collect();
    let expected: Vec<(String, PermissionDecision, DecisionSource)> = requested
        .into_iter()
        .map(|(id, _)| (id, PermissionDecision::Allow, DecisionSource::Policy))
        .collect();
    assert_eq!(resolved, expected);
    // Nobody to wait for, and nothing answered on the endpoint: each call
    // failed and the turn went on to its end.
    assert!(!events.iter().any(is_status(SessionStatus::Waiting)));
    assert_eq!(
        tool_calls(&events),
        [
            ("mcp__calin__dispatch".to_owned(), ItemStatus::Failed),
            ("mcp__calin__dispatch".to_owned(), ItemStatus::Failed),
            ("mcp__calin__send_message".to_owned(), ItemStatus::Failed),
        ]
    );
    assert!(manager.store().sessions().await.unwrap().len() == 1);
}
