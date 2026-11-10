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
The same emitted value always receives the same placeholder.
Only `result.agentInfo.version` is normalized to `<agent-version>` on both
sides, so releases do not churn the corpus; other version fields stay literal. The cwd supplied
by the scenario is normalized to a fixed marker when echoed; a different
returned cwd still fails. Non-opaque fields remain literal, including prose
that resembles a placeholder. Missing fields, changed content, identity splits
or collapses and notification reordering fail comparison.
The checked-in records contain original emitted values.

The harness also validates the corpus itself, not only the replay: every
`.jsonl` file must be referenced by an `acp_fixture::check` call, each record
must carry exactly `seq`, `dir` and `msg` with a monotonic sequence number,
`msg` must be framed JSON-RPC 2.0, and every request in either direction must
have its response. That keeps the client half, which the replay comparison
does not read, usable as the consumer's executable contract.

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

The fixture load occurs while its daemon session remains live. Close/resume
are asserted afterward, outside that recording: closing and reattaching
can race asynchronous provider-catalog refresh notifications. The shared
scenario gates provider completion on observed debug-socket queue admission,
since steer injection happens after the provider reaches its safe point.
No timer sleeps establish protocol ordering.

## Scenarios

- `01-create-list-load-v1.jsonl`: create, list, prompt and replay with attachment metadata.
- `02-shared-observe-steer-v1.jsonl`: observer adopts a peer turn; its steer result shares the bracket identity.
- `03-stop-then-prompt-v1.jsonl`: origin connection through shared work, observer close, then Stop immediately followed by a successor prompt. The successor ends normally.
- `04-detach-v1.jsonl`: originator leaves while work is gated; its prompt response precedes detach success, repeated detach succeeds, and an initialize barrier carries no further session notifications. A second connection verifies shared completion.

- `05-close-owner-aware-v1.jsonl`: observer view of owner cancellation and observer close; Rust checks correlated origin cancellation, surviving peer completion and idempotence.

- `07-config-commands-v1.jsonl`: effective model/effort options, legacy setter aliases, command discovery and selection-only correlation. An attached peer also receives the full effective options after every model/effort setter, checked in Rust.

- `08-negotiation-legacy-v1.jsonl`: future-version negotiation, full adapter metadata, extension errors, ignored prompt metadata, legacy shared-control refusal and omitted correlation. Uses the existing test-only Unix daemon stub, with no runtime capability switch.

- `09-elicitation-form-v1.jsonl`: opted-in form elicitation. This pulled-forward
  optional feature has its own transcript, separate from the mid-turn attach case.

- `09-midturn-attach-v1.jsonl`: observer attaches to a gated active turn;
  exactly one start/terminal pair preserves its identity. Replayed by the
  exhaustive 12-case interleaving test.

- `10-reverse-steer-v1.jsonl`: actual raw-daemon SoftInterrupt into an
  ACP-owned turn; peer user chunk and its message identity precede affected output.
- `11-import-repeated-user-replay-v1.jsonl`: two deliberately equal stored
  user rows imported and loaded twice; distinct IDs and grouping stay stable.
- `12-skill-selection-clear-v1.jsonl`: explicit metadata selection, textual
  selection and an explicit `skill: null` clear. Each selection-only response
  has `turnId: null` and `role: none`, and clearing emits no activation notice
  or model turn. Recorded and replayed by `acp_skill_clear_round_trip`.

Wire tests also exercise observer detach, queued-input withdrawal, adopted-steer detach,
parameter errors and observer-close brackets. All contract-gap tests are enabled.

Number 06 is intentionally absent: acceptance scenario 6 in the contract is
the deferred v2 delivery track, not a missing v1 recording.
