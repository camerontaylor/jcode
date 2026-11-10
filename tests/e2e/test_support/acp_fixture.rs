//! Ordered structural comparison; opaque values retain identity relationships.
use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn normalized(tape: &[Value]) -> Vec<Value> {
    fn visit(v: &mut Value, key: &str, ids: &mut BTreeMap<String, String>, cwd: Option<&str>) {
        match v {
            Value::String(s) if key == "cwd" && Some(s.as_str()) == cwd => {
                *s = "<scenario-cwd>".into();
            }
            Value::String(s)
                if matches!(
                    key,
                    "sessionId"
                        | "turnId"
                        | "clientInstanceId"
                        | "messageId"
                        | "updatedAt"
                        | "createdAt"
                        | "cursor"
                        | "nextCursor"
                ) =>
            {
                let n = ids.len();
                let replacement = ids
                    .entry(s.clone())
                    .or_insert_with(|| format!("<opaque-{n}>"));
                *s = replacement.clone();
            }
            Value::Array(a) => {
                for v in a {
                    visit(v, key, ids, cwd);
                }
            }
            Value::Object(o) => {
                for (k, v) in o {
                    visit(v, k, ids, cwd);
                }
            }
            _ => {}
        }
    }
    let mut ids = BTreeMap::new();
    let cwd = tape
        .iter()
        .filter(|v| v["dir"] == "c2a")
        .find_map(|v| v["msg"]["params"]["cwd"].as_str());
    tape.iter()
        .filter(|v| v["dir"] == "a2c")
        .map(|v| {
            let mut msg = v["msg"].clone();
            // Only the initialize result's agent version is release-volatile.
            if let Some(version) = msg.pointer_mut("/result/agentInfo/version")
                && version.is_string()
            {
                *version = json!("<agent-version>");
            }
            visit(&mut msg, "", &mut ids, cwd);
            msg
        })
        .collect()
}

pub(crate) fn compare(expected: &[Value], actual: &[Value]) -> Result<()> {
    let expected = normalized(expected);
    let actual = normalized(actual);
    for i in 0..expected.len().max(actual.len()) {
        if expected.get(i) != actual.get(i) {
            anyhow::bail!(
                "ACP fixture drift at a2c message {}\nexpected: {}\nactual:   {}",
                i + 1,
                expected.get(i).unwrap_or(&Value::Null),
                actual.get(i).unwrap_or(&Value::Null)
            );
        }
    }
    Ok(())
}

pub(crate) fn check(name: &str, tape: &[Value]) -> Result<()> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/acp")
        .join(name);
    if std::env::var("JCODE_ACP_FIXTURE_RECORD").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap())?;
        let lines = tape
            .iter()
            .map(serde_json::to_string)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        std::fs::write(path, lines.join("\n") + "\n")?;
        return Ok(());
    }
    let expected = std::fs::read_to_string(path)?
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<Vec<Value>, _>>()?;
    compare(&expected, tape)
}

#[test]
fn acp_conformance_comparator_identity_drift_and_order() {
    let tape = |a: &str, b: &str| {
        vec![
            json!({"dir":"a2c","msg":{"sessionId":a,"state":"started"}}),
            json!({"dir":"a2c","msg":{"sessionId":b,"state":"completed"}}),
        ]
    };
    assert!(compare(&tape("a", "a"), &tape("b", "b")).is_ok());
    assert!(compare(&tape("a", "a"), &tape("b", "c")).is_err());
    assert!(compare(&tape("a", "b"), &tape("c", "c")).is_err());
    let prose = |text: &str| vec![json!({"dir":"a2c","msg":{"text":text}})];
    assert!(compare(&prose("<opaque-0>"), &prose("<opaque-1>")).is_err());
    let mut reversed = tape("a", "a");
    reversed.reverse();
    assert!(compare(&tape("a", "a"), &reversed).is_err());
    reversed[0]["msg"]["state"] = json!("failed");
    assert!(compare(&tape("a", "a"), &reversed).is_err());
}

/// The corpus is the shared executable contract, so the replay comparison is
/// not enough: it only reads adapter-to-client messages. A consumer also
/// replays the client half, and an orphaned or truncated file would silently
/// stop being a contract. Validate the envelope and both directions for every
/// checked-in file, and require each file to be replayed by a real test.
#[test]
fn acp_corpus_files_are_well_formed_and_referenced() -> Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture_dir = root.join("tests/fixtures/acp");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&fixture_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    files.sort();
    assert!(!files.is_empty(), "the corpus has files to replay");

    let mut referenced = std::collections::BTreeSet::new();
    let mut pending = vec![root.join("tests/e2e")];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                let source = std::fs::read_to_string(&path)?;
                for fragment in source.split("acp_fixture::check(\"").skip(1) {
                    if let Some((name, _)) = fragment.split_once('"') {
                        referenced.insert(name.to_string());
                    }
                }
            }
        }
    }

    for path in &files {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| format!("corpus path is not UTF-8: {}", path.display()))?
            .to_string();
        assert!(
            referenced.contains(&name),
            "corpus file {name} is not replayed by any test"
        );
        let mut client_requests = std::collections::BTreeSet::new();
        let mut client_responses = std::collections::BTreeSet::new();
        let mut adapter_requests = std::collections::BTreeSet::new();
        let mut adapter_responses = std::collections::BTreeSet::new();
        let mut saw_client = false;
        let mut saw_adapter = false;
        for (index, line) in std::fs::read_to_string(path)?.lines().enumerate() {
            let record: Value = serde_json::from_str(line)
                .with_context(|| format!("{name}:{} is not JSON", index + 1))?;
            let object = record
                .as_object()
                .with_context(|| format!("{name}:{} is not an object", index + 1))?;
            assert_eq!(
                object.len(),
                3,
                "{name}:{} carries exactly seq, dir and msg",
                index + 1
            );
            assert_eq!(
                record["seq"].as_u64(),
                Some(index as u64 + 1),
                "{name}:{} has a monotonic sequence number",
                index + 1
            );
            let dir = record["dir"]
                .as_str()
                .with_context(|| format!("{name}:{} has a string dir", index + 1))?;
            let msg = record["msg"]
                .as_object()
                .with_context(|| format!("{name}:{} has an object msg", index + 1))?;
            assert_eq!(
                msg.get("jsonrpc").and_then(Value::as_str),
                Some("2.0"),
                "{name}:{} is a framed JSON-RPC message",
                index + 1
            );
            let id = msg
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    msg.get("id")
                        .and_then(Value::as_u64)
                        .map(|id| id.to_string())
                });
            let is_request = msg.contains_key("method") && id.is_some();
            let is_notification = msg.contains_key("method") && id.is_none();
            let is_response = !msg.contains_key("method") && id.is_some();
            assert!(
                is_request || is_notification || is_response,
                "{name}:{} is a request, notification or response",
                index + 1
            );
            match dir {
                "c2a" => {
                    saw_client = true;
                    if is_request {
                        client_requests.insert(id.clone().expect("requests carry an id"));
                    }
                    if is_response {
                        client_responses.insert(id.clone().expect("responses carry an id"));
                    }
                }
                "a2c" => {
                    saw_adapter = true;
                    if is_request {
                        adapter_requests.insert(id.clone().expect("requests carry an id"));
                    }
                    if is_response {
                        adapter_responses.insert(id.clone().expect("responses carry an id"));
                    }
                }
                other => panic!("{name}:{} has an unknown direction {other}", index + 1),
            }
        }
        assert!(saw_client && saw_adapter, "{name} records both directions");
        for id in &client_requests {
            assert!(
                adapter_responses.contains(id),
                "{name}: client request {id} has no adapter response"
            );
        }
        for id in &adapter_requests {
            assert!(
                client_responses.contains(id),
                "{name}: adapter request {id} has no client response"
            );
        }
    }
    for name in &referenced {
        assert!(
            files
                .iter()
                .any(|path| path.file_name().and_then(|file| file.to_str()) == Some(name.as_str())),
            "a test references missing corpus file {name}"
        );
    }
    Ok(())
}

#[test]
fn acp_conformance_comparator_normalizes_only_agent_info_version() {
    let tape = |version: Value| {
        vec![
            json!({"dir":"a2c","msg":{"result":{"agentInfo":{"version":version},"version":"stable"}}}),
        ]
    };
    assert!(compare(&tape(json!("1.0")), &tape(json!("2.0"))).is_ok());
    let mut other = tape(json!("2.0"));
    other[0]["msg"]["result"]["version"] = json!("changed");
    assert!(compare(&tape(json!("1.0")), &other).is_err());
    assert!(compare(&tape(json!("1.0")), &tape(json!(null))).is_err());
    let mut missing = tape(json!("1.0"));
    missing[0]["msg"]["result"]["agentInfo"]
        .as_object_mut()
        .unwrap()
        .remove("version");
    assert!(compare(&tape(json!("1.0")), &missing).is_err());
}
