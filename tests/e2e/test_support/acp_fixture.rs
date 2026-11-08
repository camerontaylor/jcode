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
    let mut reversed = tape("a", "a");
    reversed.reverse();
    assert!(compare(&tape("a", "a"), &reversed).is_err());
    reversed[0]["msg"]["state"] = json!("failed");
    assert!(compare(&tape("a", "a"), &reversed).is_err());
}
