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
The same emitted value always receives the same placeholder. The cwd supplied
by the scenario is normalized to a fixed marker when echoed; a different
returned cwd still fails. Non-opaque fields remain literal, including prose
that resembles a placeholder. Missing fields, changed content, identity splits
or collapses and notification reordering fail comparison.
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

The fixture load occurs while its daemon session remains live. Close/resume
are asserted afterward, outside that recording: closing and reattaching
can race asynchronous provider-catalog refresh notifications. The shared
scenario gates provider completion on observed debug-socket queue admission,
since steer injection happens after the provider reaches its safe point.
No timer sleeps establish protocol ordering.

## Known contract gaps

- [`acp_conformance_capability_metadata_contract`](../../e2e/acp_conformance.rs): ignored because standard initialize omits jcode capability metadata.
- [`acp_conformance_observer_close_bracket_contract`](../../e2e/acp_conformance.rs): ignored because observer close omits the required `canceled` peer terminal.
