use grok_build_search_mcp::{ErrorCode, ResponseFormat, WebSearchInput, parse_grok_json};

#[test]
fn search_rejects_empty_query() {
    let input = WebSearchInput {
        query: "  \n".to_string(),
        response_format: None,
    };

    let error = input.validate().expect_err("empty query must fail");
    assert_eq!(error.code, ErrorCode::InvalidQuery);
}

#[test]
fn search_rejects_query_over_8000_characters() {
    let input = WebSearchInput {
        query: "界".repeat(8_001),
        response_format: None,
    };

    let error = input.validate().expect_err("oversized query must fail");
    assert_eq!(error.code, ErrorCode::InvalidQuery);
}

#[test]
fn concise_grok_json_is_parsed_without_thought_and_deduplicates_sources() {
    let raw = serde_json::json!({
        "text": "Answer. https://example.com/a and [same](https://example.com/a). See https://www.rust-lang.org/learn.",
        "stopReason": "end_turn",
        "sessionId": "session-1",
        "requestId": "request-1",
        "thought": "must never escape"
    })
    .to_string();

    let output = parse_grok_json(&raw, ResponseFormat::Concise).expect("valid Grok JSON");

    assert!(output.ok);
    assert!(output.verified);
    assert_eq!(output.session_id.as_deref(), Some("session-1"));
    assert_eq!(output.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(output.sources.len(), 2);
    assert!(
        !serde_json::to_string(&output)
            .unwrap()
            .contains("must never escape")
    );
}

#[test]
fn response_is_truncated_on_character_boundary() {
    let raw = serde_json::json!({
        "text": format!("{} https://example.com/source", "界".repeat(12_100)),
        "stopReason": "end_turn",
        "sessionId": "session-2",
        "requestId": "request-2"
    })
    .to_string();

    let output = parse_grok_json(&raw, ResponseFormat::Concise).expect("valid Grok JSON");

    assert!(output.truncated);
    assert_eq!(output.answer.chars().count(), 12_000);
}

#[test]
fn search_output_without_public_sources_is_rejected() {
    let raw = serde_json::json!({
        "text": "An uncited answer.",
        "stopReason": "end_turn",
        "sessionId": "session-3",
        "requestId": "request-3"
    })
    .to_string();

    let error = parse_grok_json(&raw, ResponseFormat::Detailed)
        .expect_err("uncited search must not be verified");

    assert_eq!(error.code, ErrorCode::NoSources);
}

#[test]
fn search_output_with_only_private_sources_is_rejected() {
    let raw = serde_json::json!({
        "text": "Internal source: http://127.0.0.1/admin and http://api.localhost/data",
        "stopReason": "end_turn",
        "sessionId": "session-private",
        "requestId": "request-private"
    })
    .to_string();

    let error = parse_grok_json(&raw, ResponseFormat::Concise)
        .expect_err("private URLs must not verify a search result");

    assert_eq!(error.code, ErrorCode::NoSources);
}
