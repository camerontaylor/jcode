# ACP v1 wire corpus

These transcripts come from the real `jcode acp` stdio child, connected to
an isolated daemon using a scripted mock provider. Each JSON Lines record is
`{"seq":1,"dir":"c2a","msg":{...}}` or the corresponding `a2c` record.
Sequence numbers count send/read observations on a single connection.
The shared scenario records the observer connection (which later steers);
the originator is driven by the Rust scenario.

Normal runs execute the scenarios again and structurally compare every
adapter-to-client message in order. Object key ordering is insignificant;
array and notification ordering is significant. JSON-RPC request IDs remain
literal to verify request correlation. Opaque string fields `sessionId`,
`turnId`, `clientInstanceId`, `messageId`, `updatedAt`, `createdAt`,
`cursor`, and `nextCursor` receive placeholders consistently across a file.
The same emitted value always receives the same placeholder. The cwd supplied by the scenario is normalized to a fixed marker when echoed;
a different returned cwd still fails. Missing fields,
changed content, identity splits and notification reordering fail comparison.
The checked-in records contain original emitted values.

Run on a development host with sufficient memory, following AGENTS.md:

```sh
CARGO_NET_GIT_FETCH_WITH_CLI=true systemd-run --user --scope -p MemoryMax=6G nice -n 10 cargo test -j 3 --test e2e acp_conformance
```

To deliberately refresh the corpus, prefix that command with
`JCODE_ACP_FIXTURE_RECORD=1`. Review the JSONL diff alongside any wire
change. Recording is opt-in; normal runs never rewrite files. The first
differing adapter message is printed on drift.

Malformed input is tested separately: the adapter emits a null-ID parse
error (`-32700`) and continues reading. It cannot be represented as a
JSON object in this corpus.

## Validation on triton

Five consecutive normal runs passed: each reported 5 passed, 2 ignored,
with durations 0.55, 0.50, 0.49, 0.49 and 0.52 seconds.
The suite contains:

- `acp_conformance_framing_ids_and_errors`: numeric/string IDs, initialize
  fields, unknown methods/extensions, invalid parameters, legacy unknown
  session errors, malformed JSON recovery and strict JSON-line stdout.
- `acp_conformance_create_list_load`: missing/null/array MCP input, idle
  cancellation, streamed prompt completion, list membership, replay before
  load response, idempotent close and resume without replay.
- `acp_conformance_list_metadata_and_pagination`: 53 stored sessions,
  titles, RFC 3339 updatedAt, newest-first traversal and opaque cursor use
  without duplicate or missing entries.
- `acp_conformance_shared_observe_steer`: peer start/output/terminal order,
  one shared turn and two prompt responses, no duplicate text, observer close
  preserving origin work, active cancellation and idle-stop successor safety.
- `acp_conformance_comparator_identity_drift_and_order`: consistent opaque
  identity normalization, identity/content drift and reordered notifications.

Two target-contract tests are ignored, and an explicit ignored-test run
confirmed both failures on this branch:

- `acp_conformance_capability_metadata_contract`: standard initialize
  omits `agentCapabilities._meta.jcode`; interfaceVersion was null instead
  of 1. See `initialize_result` in `src/cli/acp.rs`.
- `acp_conformance_observer_close_bracket_contract`: close returns without
  the required canceled peer terminal; one bracket notification (start)
  was observed instead of two. Origin work still completes. See
  `handle_session_close` and router `close_view`.

The fixture load occurs while its daemon session remains live. Close/resume
are asserted afterward, outside that recording: closing and reattaching
can race asynchronous provider-catalog refresh notifications. The shared
scenario gates provider completion on observed debug-socket queue admission,
since steer injection happens after the provider reaches its safe point.
No timer sleeps establish protocol ordering.

Commands used (logs redirected to bounded local files):

```sh
hostname
free -m
nice -n 10 cargo test -j 3 --test e2e acp_conformance --no-run
CARGO_NET_GIT_FETCH_WITH_CLI=true systemd-run --user --scope -p MemoryMax=6G nice -n 10 cargo test -j 3 --test e2e acp_conformance --no-run
JCODE_ACP_FIXTURE_RECORD=1 CARGO_NET_GIT_FETCH_WITH_CLI=true systemd-run --user --scope -p MemoryMax=6G nice -n 10 cargo test -j 3 --test e2e acp_conformance
for run in 1 2 3 4 5; do
  CARGO_NET_GIT_FETCH_WITH_CLI=true systemd-run --user --scope -p MemoryMax=6G nice -n 10 cargo test -j 3 --test e2e acp_conformance > /tmp/acp-conformance-run-$run.log 2>&1 || break
done
CARGO_NET_GIT_FETCH_WITH_CLI=true systemd-run --user --scope -p MemoryMax=6G nice -n 10 cargo test -j 3 --test e2e acp_conformance -- --ignored
git diff --check
```

The initial default git fetch failed SSH authentication; CLI fetching worked.
The first compilation then required installing `pkg-config libssl-dev`
with `sudo -n apt-get install -y pkg-config libssl-dev`. No production code
was changed. Full suites were not run.
