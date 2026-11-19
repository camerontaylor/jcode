use super::*;
use crate::mcp::InitializeResult;

fn init_result(extra: &str) -> InitializeResult {
    serde_json::from_str(&format!(
        r#"{{"protocolVersion":"2025-06-18","capabilities":{{}},
            "serverInfo":{{"name":"s","version":"1"}}{extra}}}"#
    ))
    .expect("initialize result parses")
}

#[test]
fn initialize_result_reads_string_instructions() {
    let result = init_result(r##","instructions":"Call search first.""##);
    assert_eq!(result.instructions.as_deref(), Some("Call search first."));
}

#[test]
fn initialize_result_without_instructions_is_none() {
    assert_eq!(init_result("").instructions, None);
    assert_eq!(init_result(r#","instructions":null"#).instructions, None);
}

#[test]
fn initialize_result_ignores_non_string_instructions() {
    for value in ["42", "true", r#"["a"]"#, r#"{"text":"a"}"#] {
        let result = init_result(&format!(r#","instructions":{value}"#));
        assert_eq!(result.instructions, None, "value {value}");
        assert_eq!(result.protocol_version, "2025-06-18");
    }
}

#[test]
fn cap_trims_and_drops_blank_text() {
    assert_eq!(cap_server_instructions("  \n "), None);
    assert_eq!(
        cap_server_instructions("\n hello \n").as_deref(),
        Some("hello")
    );
}

#[test]
fn cap_keeps_text_at_the_limit_and_truncates_beyond_it() {
    let exact = "a".repeat(MAX_SERVER_INSTRUCTIONS_CHARS);
    assert_eq!(cap_server_instructions(&exact), Some(exact.clone()));

    // Multi-byte characters: the cap counts characters, never splits one.
    let long = "é".repeat(MAX_SERVER_INSTRUCTIONS_CHARS + 10);
    let capped = cap_server_instructions(&long).expect("capped");
    assert_eq!(capped.chars().count(), MAX_SERVER_INSTRUCTIONS_CHARS);
    assert!(capped.ends_with("[instructions truncated]"));
}

#[test]
fn section_is_none_without_instructions() {
    assert_eq!(build_instructions_section(&[]), None);
    assert_eq!(
        build_instructions_section(&[("blank".into(), "   ".into())]),
        None
    );
}

#[test]
fn section_labels_each_server_in_name_order() {
    let section = build_instructions_section(&[
        ("zeta".into(), "Zeta guidance.".into()),
        ("alpha".into(), "Alpha guidance.".into()),
        ("empty".into(), "".into()),
    ])
    .expect("section");
    assert!(section.starts_with("# MCP Server Instructions\n\n"));
    assert!(section.contains("not from the user"));
    let alpha = section.find("## alpha\nAlpha guidance.").expect("alpha");
    let zeta = section.find("## zeta\nZeta guidance.").expect("zeta");
    assert!(alpha < zeta);
    assert!(!section.contains("## empty"));
}

#[test]
fn section_caps_each_server_independently() {
    let big = "x".repeat(MAX_SERVER_INSTRUCTIONS_CHARS * 2);
    let section =
        build_instructions_section(&[("big".into(), big), ("small".into(), "Small.".into())])
            .expect("section");
    let body = section
        .split("## big\n")
        .nth(1)
        .and_then(|rest| rest.split("\n\n## small").next())
        .expect("big body");
    assert_eq!(body.chars().count(), MAX_SERVER_INSTRUCTIONS_CHARS);
    assert!(section.ends_with("## small\nSmall."));
}
