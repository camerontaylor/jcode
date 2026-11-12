// Read-only production sample, 2026-11-08 UTC: Swan snapshot 97,722,380 bytes,
// journal 264,881 bytes. Ran this exact embedded projector over existing managed
// SSH (python3 stdin only), three samples per mode. Snapshot/journal size and
// mtime_ns were unchanged before/after each sample. No daemon RPC or host writes.
// Preview: 80 messages, 181,277 output bytes including framing/telemetry.
// Remote projection ms: 2465.50, 1862.45, 1646.69.
// End-to-end fresh SSH ms: 8923.53, 5664.24, 7751.36.
// Local Python json.loads median ms (20 parses/sample): 0.969, 1.121, 0.904.
// This measures JSON decoding only, NOT Rust parse_preview/hydration. The prior
// ~49 MB cache/~300 ms parse baseline is therefore not an apples-to-apples timing
// comparison, though the transferred projection is now bounded and much smaller.
// Inventory: 278 output bytes; remote ms: 912.64, 1453.10, 736.12.
// End-to-end fresh SSH ms: 4342.86, 4807.31, 4282.34.
// Prior 97 MB inventory baseline was 702 ms: no inventory speedup demonstrated.
// Reproduction script/results: ~/.jcode/scratch/bounded-preview-measure.py and
// ~/.jcode/scratch/bounded-preview-measure-results.jsonl on the measurement host.
// Scanner follow-up, 2026-11-08 UTC, same unchanged 97,722,380-byte snapshot:
// Whole-string regex alone did not improve latency. Coalescing scalar tokens
// plus retaining the last 80 message ranges during the first scan avoids the
// second full messages scan. Actual mmap preview remote ms: 768.34, 1381.58,
// 1376.54, fresh SSH ms: 4695.30, 5622.85, 5616.78. All retain 80 messages.
// Journal grew externally between samples (363,579 -> 384,539 bytes), but each
// individual read verified snapshot/journal size+mtime unchanged during projection.
// Paired comparison freezes both files in process RAM, no remote file writes:
// old preview ms 1419.64, 2489.48, 1444.72; new 1315.81, 1160.17, 772.47.
// Every pair is byte-identical (168,621 bytes, without framing/telemetry), SHA256
// cc39e053c4a2e80dd767fee2a4142f784a44c2cd10d6685a97493d5a04cf75fc.
// Inventory actual mmap ms 1019.11, 1507.74, 742.82: no consistent speedup.
// Evidence: ~/.jcode/scratch/bounded-preview-final-scanner-results.jsonl and
// paired-projection-final-results.txt, reproduced by paired-projection.py.
// Journal follow-up, 2026-11-08 UTC, synthetic LOCAL inputs only, no cloud reads:
// 34,662,320-byte frozen mmap journal, 220 entries x 120 ordinary messages with
// escaped quotes/backslashes, braces and Unicode. Alternating before/after order,
// one warmup then six measured runs per mode. Removing only the redundant outer
// journal-entry scan: preview medians 895.38 -> 660.65 ms (-26.2%), inventory
// 566.62 -> 284.27 ms (-49.8%). Every paired JSON output was byte-identical.
// Snapshot/journal size, mtime_ns and SHA256 were unchanged across all runs.
// Journal SHA256 aa9a2e05ed8a7500ed42fdf832b68995d488814752ec6abd69a5af2f91e4dafd.
// Local reproduction: ~/.jcode/scratch/journal-paired.py, journal-before.py and
// journal-paired-results.txt. This does not establish live-cloud/SSH latency.
use super::*;
use std::io::Write;

#[test]
fn remote_timing_errors_and_skips_are_safe_and_do_not_change_wire_format() {
    let home = tempfile::tempdir().unwrap();
    let private_id = "private_session_identifier";
    let preview = projection_output(home.path(), "preview", private_id);
    assert!(!preview.status.success());
    assert!(preview.stdout.is_empty());
    let inventory = run_projection(
        home.path(),
        "inventory",
        &format!("{private_id}\n../private"),
    );
    assert!(inventory.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&inventory.stdout).unwrap();
    assert_eq!(payload["sessions"], serde_json::json!([]));
    for output in [&preview, &inventory] {
        let logs = String::from_utf8_lossy(&output.stderr);
        assert!(logs.contains("stage=projection outcome=error duration_ms="));
        assert!(!logs.contains(private_id));
        assert!(!logs.contains(home.path().to_str().unwrap()));
        assert!(!logs.contains("Traceback"));
        for line in logs.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 4, "{line}");
            assert_eq!(fields[0], "[CLOUD_TIMING]");
            assert!(fields[1].starts_with("stage="));
            assert!(fields[2].starts_with("outcome="));
            fields[3]
                .strip_prefix("duration_ms=")
                .unwrap()
                .parse::<u64>()
                .unwrap();
        }
    }
    assert!(String::from_utf8_lossy(&inventory.stderr).contains("outcome=skipped_invalid_target"));
    std::fs::create_dir(home.path().join("sessions")).unwrap();
    let session = Session::create_with_id(private_id.into(), None, Some("private title".into()));
    std::fs::write(
        home.path()
            .join("sessions")
            .join(format!("{private_id}.json")),
        serde_json::to_vec(&session).unwrap(),
    )
    .unwrap();
    let success = run_projection(home.path(), "preview", private_id);
    assert!(success.status.success());
    assert!(parse_for_session(&success.stdout, private_id).is_some());
    let logs = String::from_utf8_lossy(&success.stderr);
    assert!(logs.contains("stage=projection outcome=success duration_ms="));
    assert!(!logs.contains(private_id));
    assert!(!logs.contains("private title"));
}

#[test]
fn remote_scanner_handles_escaped_nested_strings_and_rejects_truncation() {
    let checks = r#"
import random
random.seed(37)
samples = ['', '"', '\\', '\\"', '\\\\"', '{}[]', '\n\t', '雪😀']
for _ in range(200):
    samples.append(''.join(random.choices('abc"\\{}[]\n雪', k=120)))
for text in samples:
    value = {'nested': [{'text': text, 'braces': ']}{['}, [text]], 'tail': text}
    encoded = json.dumps(value, ensure_ascii=False).encode()
    assert value_end(encoded + b',trailer', 0) == len(encoded)
    quoted = json.dumps(text, ensure_ascii=False).encode()
    assert string_end(quoted + b',trailer', 0) == len(quoted)
    assert json.loads(encoded) == value
messages = [{'id': str(i), 'content': [{'text': samples[i % len(samples)]}]} for i in range(300)]
encoded = json.dumps({'messages': messages}).encode()
ranges = collections.deque(maxlen=MAX_MESSAGES)
list(fields(encoded, message_ranges=ranges))
assert len(ranges) == MAX_MESSAGES
assert [json.loads(encoded[a:b]) for a,b in ranges] == messages[-MAX_MESSAGES:]
encoded = b'{"messages":[{"id":"old"}],"messages":[]}'
for key,a,b in fields(encoded, message_ranges=ranges):
    pass
assert not ranges
# Compare exact field spans with the former outer-container scan, including
# whitespace, adjacent entries, duplicate keys, scalars and escaped braces.
def previous_journal_fields(data):
    pos = 0
    while pos < len(data):
        pos = skip_space(data, pos)
        if pos == len(data):
            break
        end = value_end(data, pos)
        yield from fields(data, pos)
        pos = end
entries = [b'{}', b'{"meta":{"title":"first","title":"last"}}',
           b'{"append_messages":[null,1,"escaped\\\"{}[]",{},[]],"append_messages":[]}',
           json.dumps({'unknown': [{'nested': samples}]}).encode()]
for separator in (b'', b'\n', b' \r\n\t'):
    encoded = separator + separator.join(entries) + separator
    assert list(journal_fields(encoded)) == list(previous_journal_fields(encoded))
for malformed in (b'{', b'{"x":[]]', b'{"x":"unterminated}'):
    assert list(journal_fields(malformed)) == []
# A complete first entry survives a torn second entry on the same line.
assert list(journal_fields(b'{"meta":{}} {')) == list(journal_fields(b'{"meta":{}}'))
import tempfile
with tempfile.TemporaryDirectory() as directory:
    path = Path(directory) / 'journal.json'
    path.write_bytes(b'{"id":"journal","messages":[],"route_api_method":"original"}')
    journal = path.with_suffix('.journal.jsonl')
    journal.write_bytes(b'{"meta":{"title":"first","title":"last","route_api_method":"ignored"},"append_messages":[null,1,"scalar"]}'
                        b'{"meta":{"route_api_method_present":true,"route_api_method":"responses"},"append_messages":[]}'
                        b'{"meta":{"route_api_method_present":true,"route_api_method_present":false,"route_api_method":"ignored"}} \n')
    before = [(p.stat().st_size, p.stat().st_mtime_ns) for p in (path, journal)]
    result = project(path, True)
    assert result['title'] == 'last'
    assert result['route_api_method'] == 'responses'
    assert result['messages'] == [None, 1, 'scalar']
    assert all(result[k] == [] for k in ('env_snapshots', 'memory_injections', 'replay_events'))
    inventory = project(path, False)
    assert inventory['title'] == 'last' and 'messages' not in inventory
    assert before == [(p.stat().st_size, p.stat().st_mtime_ns) for p in (path, journal)]
    for damaged in (b'{"meta":{"title":"discard"},', b'not json', b'{"meta":]', b'{"meta":{"title":garbage}}'):
        journal.write_bytes(damaged + b'\n' + b'{"meta":{"title":"recovered"}}\n' + b'{"append_messages":[')
        for preview in (True, False):
            result = project(path, preview)
            assert result['title'] == 'recovered'
    # A torn append without newline can be followed by complete canonical
    # entries. Recover both metadata and messages, as Rust journal replay does.
    first = b'{"meta":{"title":"salvaged"},"append_messages":[{"role":"user","content":[{"type":"text","text":"kept"}]}]}'
    second = b'{"meta":{"title":"newest"},"append_messages":[]}'
    for torn in (b'{"meta":{', b'garbage', b'{"meta":{"title":"torn'):
        journal.write_bytes(torn + first + second + b'\n')
        for preview in (True, False):
            result = project(path, preview)
            assert result['title'] == 'newest'
            if preview:
                assert result['messages'][0]['content'][0]['text'] == 'kept'


for malformed in (b'{', b'[', b'{"x":"unterminated}', b'["escaped\\"', b'{]', b'[{]}'):
    try:
        value_end(malformed, 0)
    except (ValueError, IndexError):
        pass
    else:
        raise AssertionError(repr(malformed))
"#;
    let mut child = std::process::Command::new("python3")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let definitions = REMOTE_PROJECTOR.split("if __name__ ==").next().unwrap();
    let mut input = child.stdin.take().unwrap();
    write!(input, "{definitions}\n{checks}").unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_projection(home: &std::path::Path, mode: &str, id: &str) -> std::process::Output {
    let output = projection_output(home, mode, id);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn projection_output(home: &std::path::Path, mode: &str, id: &str) -> std::process::Output {
    let mut child = std::process::Command::new("python3")
        .args(["-", mode, id])
        .env("JCODE_HOME", home)
        .env_remove("JCODE_RUNTIME_DIR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("Python is required by the remote projection contract");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(REMOTE_PROJECTOR.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn oversized_remote_snapshot_is_bounded_valid_and_keeps_late_journal_updates() {
    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join("sessions");
    std::fs::create_dir(&sessions).unwrap();
    let id = "oversized_preview";
    let session = Session::create_with_id(id.into(), None, Some("Old title".into()));
    let mut snapshot = serde_json::to_value(&session).unwrap();
    snapshot["migration_epoch"] = serde_json::json!(19);
    snapshot["messages"] = serde_json::json!([{
        "id": "huge", "role": "user", "content": [{"type": "text", "text": "x".repeat(65 * 1024 * 1024)}]
    }]);
    let path = sessions.join(format!("{id}.json"));
    serde_json::to_writer(std::fs::File::create(&path).unwrap(), &snapshot).unwrap();
    drop(snapshot);
    let before = std::fs::metadata(&path).unwrap();
    assert!(before.len() > 64 * 1024 * 1024);
    let first_preview = run_projection(home.path(), "preview", id);
    assert!(first_preview.stdout.len() < 2 * 1024 * 1024);
    let (first, _) = parse_preview(&first_preview.stdout)
        .expect("oversized individual message projection must remain valid");
    assert_eq!(first.messages.len(), 1);
    assert_eq!(first.messages[0].id, "huge");
    let mut journal = std::fs::File::create(sessions.join(format!("{id}.journal.jsonl"))).unwrap();
    for i in 0..100 {
        writeln!(journal, "{}", serde_json::json!({
            "meta": {"title": format!("Late journal title {i}"), "working_dir": "/latest/repository", "updated_at": "2026-11-08T05:00:00Z"},
            "append_messages": [{"id": format!("late-{i}"), "role": "user", "content": [{"type": "text", "text": format!("Late journal prompt {i} escaped quote \" slash \\ nested {{[}}] 雪")}]}]
        })).unwrap();
    }
    drop(journal);
    let preview = run_projection(home.path(), "preview", id);
    assert!(preview.stdout.len() < 2 * 1024 * 1024);
    let (loaded, _) =
        parse_preview(&preview.stdout).expect("projection must remain valid Session JSON");
    assert_eq!(loaded.title.as_deref(), Some("Late journal title 99"));
    assert_eq!(loaded.messages.len(), 80);
    assert_eq!(loaded.messages.last().unwrap().id, "late-99");
    assert_eq!(loaded.migration_epoch, 19);
    assert!(build_messages_preview(&loaded).iter().any(|m| {
        m.content
            .contains("Late journal prompt 99 escaped quote \" slash \\ nested {[}] 雪")
    }));
    let inventory = run_projection(home.path(), "inventory", id);
    let inventory: serde_json::Value = serde_json::from_slice(&inventory.stdout).unwrap();
    assert_eq!(inventory["sessions"][0]["title"], "Late journal title 99");
    assert_eq!(inventory["sessions"][0]["cwd"], "/latest/repository");
    assert_eq!(inventory["sessions"][0]["epoch"], 19);
    assert_eq!(
        inventory["sessions"][0]["updated_at"],
        "2026-11-08T05:00:00Z"
    );
    assert_eq!(std::fs::metadata(&path).unwrap().len(), before.len());
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        before.modified().unwrap()
    );
    assert_eq!(std::fs::read_dir(&sessions).unwrap().count(), 2);
}

#[test]
fn remote_projection_ignores_stale_sidecar_and_respects_away_lease() {
    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join("sessions");
    std::fs::create_dir(&sessions).unwrap();
    let session = Session::create_with_id("authority".into(), None, Some("Canonical title".into()));
    serde_json::to_writer(
        std::fs::File::create(sessions.join("authority.json")).unwrap(),
        &session,
    )
    .unwrap();
    std::fs::write(
        sessions.join("authority.meta.json"),
        br#"{"title":"Stale sidecar","migration_epoch":999}"#,
    )
    .unwrap();
    let result = run_projection(home.path(), "inventory", "authority");
    let result: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result["sessions"][0]["title"], "Canonical title");
    assert_eq!(result["sessions"][0]["epoch"], 0);
    std::fs::create_dir(home.path().join("session_leases")).unwrap();
    std::fs::write(
        home.path().join("session_leases/authority.json"),
        br#"{"away_host":"other-owner","epoch":12}"#,
    )
    .unwrap();
    let result = run_projection(home.path(), "inventory", "authority");
    let result: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(result["sessions"].as_array().unwrap().is_empty());
}

#[test]
fn remote_journal_dedupe_matches_rust_checkpoint_replay() {
    use serde_json::{Value, json};
    fn message(id: usize) -> Value {
        json!({"id":id.to_string(),"role":"user","content":[{"type":"text","text":id.to_string()}]})
    }
    fn entry(snapshot: &Value, ids: &[usize], title: &str, stamp: &str) -> String {
        let mut meta = snapshot.clone();
        meta["title"] = json!(title);
        meta["model"] = json!(title);
        meta["working_dir"] = json!(format!("/{title}"));
        meta["updated_at"] = json!(stamp);
        // Match the canonical struct field order used by Rust glued salvage.
        format!(
            "{{\"meta\":{},\"append_messages\":{}}}",
            meta,
            serde_json::to_string(&ids.iter().copied().map(message).collect::<Vec<_>>()).unwrap()
        )
    }
    let id = "dedupe_parity";
    let session = Session::create_with_id(id.into(), None, Some("snapshot-new".into()));
    let mut snapshot = serde_json::to_value(session).unwrap();
    snapshot["updated_at"] = json!("2026-01-01T00:00:10.123456789Z");
    snapshot["model"] = json!("snapshot-new");
    snapshot["working_dir"] = json!("/snapshot-new");
    snapshot["messages"] = json!((1..=100).map(message).collect::<Vec<_>>());
    let old = "2026-01-01T00:00:01Z";
    let fresh = "2026-01-01T00:00:11Z";
    let stale = entry(&snapshot, &(1..=50).collect::<Vec<_>>(), "old", old);
    let overlap = entry(
        &snapshot,
        &[95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 101, 105],
        "fresh",
        fresh,
    );
    let metadata = entry(&snapshot, &[], "metadata", "2026-01-01T00:00:10.123456790Z");
    let cases = [
        ("stale", format!("{stale}\n"), "snapshot-new", 100),
        (
            "mixed-overlap",
            format!("{stale}\n{overlap}\n"),
            "fresh",
            105,
        ),
        (
            "stale-ids-new-time",
            entry(&snapshot, &[1, 2], "old", "2099-01-01T00:00:00Z"),
            "snapshot-new",
            100,
        ),
        (
            "metadata-equal-offset",
            entry(&snapshot, &[], "old", "2026-01-01T01:00:10.123456789+01:00"),
            "snapshot-new",
            100,
        ),
        (
            "metadata-nanosecond",
            format!("{stale}\n{metadata}\n"),
            "metadata",
            100,
        ),
        (
            "metadata-after-fresh",
            format!("{overlap}\n{}\n", entry(&snapshot, &[], "after", old)),
            "after",
            105,
        ),
        (
            "glued-torn-prefix",
            format!("{{\"meta\":{{{overlap}\n"),
            "fresh",
            105,
        ),
        (
            "malformed-and-torn-tail",
            format!("{stale}\nnot-json\n{overlap}\n{{\"meta\":"),
            "fresh",
            105,
        ),
    ];
    let mut cases: Vec<_> = cases
        .into_iter()
        .map(|(name, journal, title, count)| (name, journal, title, count, 100))
        .collect();
    let new = entry(&snapshot, &(101..=110).collect::<Vec<_>>(), "fresh", fresh);
    cases.extend([
        ("no-journal", String::new(), "snapshot-new", 100, 100),
        ("empty-no-journal", String::new(), "snapshot-new", 0, 0),
        (
            "empty-old-metadata",
            entry(&snapshot, &[], "old", old),
            "snapshot-new",
            0,
            0,
        ),
        ("empty-new-metadata", metadata.clone(), "metadata", 0, 0),
        (
            "empty-unseen-ids-old-time",
            entry(&snapshot, &[1, 2], "unseen", old),
            "unseen",
            2,
            0,
        ),
        (
            "new-after-stale",
            format!("{stale}\n{new}\n"),
            "fresh",
            110,
            100,
        ),
        (
            "subset-noncontiguous",
            entry(&snapshot, &[99, 1, 50], "old", fresh),
            "snapshot-new",
            100,
            100,
        ),
        (
            "duplicates-after-fresh",
            format!("{overlap}\n{}\n", entry(&snapshot, &[1, 101], "after", old)),
            "after",
            105,
            100,
        ),
        (
            "salvaged-stale-prefix",
            format!("{{\"meta\":{{{stale}{new}\n"),
            "fresh",
            110,
            100,
        ),
    ]);
    for (case, journal, title, count, initial_count) in cases {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join("sessions");
        std::fs::create_dir(&sessions).unwrap();
        let path = sessions.join(format!("{id}.json"));
        let journal_path = path.with_extension("journal.jsonl");
        let mut snapshot = snapshot.clone();
        if initial_count == 0 {
            snapshot["messages"] = json!([]);
        }
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        if !journal.is_empty() {
            std::fs::write(&journal_path, journal.as_bytes()).unwrap();
        }
        let rust = Session::load_from_path(&path).unwrap();
        assert_eq!(rust.messages.len(), count, "Rust oracle must replay {case}");
        assert_eq!(
            rust.title.as_deref(),
            Some(title),
            "Rust oracle metadata {case}"
        );
        let (projected, _) =
            parse_preview(&run_projection(home.path(), "preview", id).stdout).unwrap();
        let expected: Vec<_> = rust
            .messages
            .iter()
            .rev()
            .take(80)
            .rev()
            .map(|m| m.id.as_str())
            .collect();
        let actual: Vec<_> = projected.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(actual, expected, "message order for {case}");
        assert_eq!(projected.title, rust.title, "preview title for {case}");
        assert_eq!(projected.model, rust.model, "preview model for {case}");
        assert_eq!(
            projected.updated_at, rust.updated_at,
            "preview timestamp for {case}"
        );
        let output = run_projection(home.path(), "inventory", id);
        let inventory: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            inventory["sessions"][0]["title"],
            json!(rust.title),
            "inventory {case}"
        );
        assert_eq!(
            inventory["sessions"][0]["cwd"],
            json!(rust.working_dir),
            "inventory cwd {case}"
        );
        let stamp: chrono::DateTime<chrono::Utc> =
            serde_json::from_value(inventory["sessions"][0]["updated_at"].clone()).unwrap();
        assert_eq!(stamp, rust.updated_at, "inventory timestamp {case}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "snapshot changed for {case}"
        );
        if journal.is_empty() {
            assert!(!journal_path.exists(), "journal created for {case}");
        } else {
            assert_eq!(
                std::fs::read(&journal_path).unwrap(),
                journal.as_bytes(),
                "journal changed for {case}"
            );
        }
    }
}

fn run_projector_checks(checks: &str) {
    let mut child = std::process::Command::new("python3")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let definitions = REMOTE_PROJECTOR.split("if __name__ ==").next().unwrap();
    let mut input = child.stdin.take().unwrap();
    write!(input, "{definitions}\n{checks}").unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn remote_timestamp_uses_microsecond_parser_without_losing_nanoseconds() {
    run_projector_checks(
        r#"
import tempfile
real_datetime = datetime
class MicrosecondParser:
    # Python 3.9/3.10's fromisoformat accepts only 3/6 fractional digits.
    @staticmethod
    def fromisoformat(value):
        fraction = re.search(r'\.(\d+)', value)
        if fraction and len(fraction.group(1)) not in (3, 6):
            raise ValueError('unsupported fractional precision')
        return real_datetime.fromisoformat(value)
datetime = MicrosecondParser
try:
    for fraction, nanos in [('1',100000000), ('123',123000000), ('123456',123456000), ('123456789',123456789)]:
        value = timestamp('2026-01-01T00:00:10.' + fraction + 'Z')
        assert value is not None and value[1] == nanos, (fraction, value)
    assert timestamp('2026-01-01T00:00:10.123456789Z') == timestamp('2026-01-01T01:00:10.123456789+01:00')
    assert timestamp('2026-01-01T00:00:10.123456788Z') < timestamp('2026-01-01T00:00:10.123456789Z')
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory)/'compat.json'
        path.write_text(json.dumps({'id':'compat','title':'snapshot','updated_at':'2026-01-01T00:00:10.123456789Z','messages':[]}))
        path.with_suffix('.journal.jsonl').write_text(json.dumps({'meta':{'title':'stale','updated_at':'2026-01-01T00:00:10.123456788Z'},'append_messages':[]})+'\n')
        for preview in (True,False):
            assert project(path,preview)['title'] == 'snapshot'
finally:
    datetime = real_datetime
"#,
    );
}

#[test]
fn remote_journal_candidates_and_replay_share_checkpoint_view() {
    run_projector_checks(
        r#"
import tempfile
original_collect = journal_message_ids
original_open = Path.open
message = lambda i: {'id':str(i),'role':'user','content':[{'type':'text','text':str(i)}]}
for mutation in ('append','replace'):
    for preview in (True,False):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'race.json'
            journal = path.with_suffix('.journal.jsonl')
            before = {'id':'race','title':'snapshot','updated_at':'2026-01-01T00:00:10Z','messages':[message(i) for i in range(1,101)]}
            old_entry = json.dumps({'meta':{'title':'old','updated_at':'2026-01-01T00:00:01Z'},'append_messages':[message(1)]})+'\n'
            new_entry = json.dumps({'meta':{'title':'late','updated_at':'2026-01-01T00:00:11Z'},'append_messages':[message(101)]})+'\n'
            path.write_text(json.dumps(before)); journal.write_text(old_entry)
            opens = []
            def tracked_open(target, *args, **kwargs):
                if target == journal and args and args[0] == 'rb':
                    opens.append(target)
                return original_open(target, *args, **kwargs)
            def checkpoint_after_candidates(source):
                result = original_collect(source)
                # Deterministic writer barrier: publish a new snapshot containing
                # a message appended after the candidate mapping was captured.
                before['messages'].append(message(101))
                before['title'] = 'checkpoint'
                before['updated_at'] = '2026-01-01T00:00:12Z'
                path.write_text(json.dumps(before))
                if mutation == 'append':
                    with journal.open('a') as writer: writer.write(new_entry)
                else:
                    replacement = journal.with_suffix('.replacement')
                    replacement.write_text(old_entry + new_entry)
                    replacement.replace(journal)
                return result
            Path.open = tracked_open
            journal_message_ids = checkpoint_after_candidates
            try:
                result = project(path,preview)
            finally:
                Path.open = original_open
                journal_message_ids = original_collect
            assert result['title'] == 'checkpoint', (mutation,preview,result['title'])
            assert result['updated_at'] == '2026-01-01T00:00:12Z'
            if preview:
                assert [m['id'] for m in result['messages']] == [str(i) for i in range(22,102)]
            assert len(opens) == 1, opens
            assert json.loads(path.read_text()) == before
            assert journal.read_text() == old_entry + new_entry
"#,
    );
}
