# ACP turn correlation and detach verification

Host: triton. Branch: `fork/acp-turn-correlation-detach`, based on `fork/acp-stage`.
No installation, shared daemon restart, merge or force-push was performed.

## Staged baseline

`ec59cfa7f4` compiled the staged lanes without source reconciliation being necessary.
Enabled the capability metadata test and re-recorded both existing fixtures.
After cwd and opaque-identity normalization, only new `_meta.jcode` fields differed:
initialize capability metadata and new/load/resume attachment metadata.
Normal replay passed: 6 tests, one then-known observer-close gap ignored;
shared-session tests: 2 passed; ACP shape/unit tests: 51 passed.

## Wire exchanges

IDs below are illustrative opaque IDs; JSON shapes match the wire assertions.

Origin and inserted steer results refer to the same execution:

```json
{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn","_meta":{"jcode":{"turnId":"t1","role":"origin"}}}}
{"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn","_meta":{"jcode":{"turnId":"t1","role":"steer"}}}}
{"jsonrpc":"2.0","id":5,"result":{"stopReason":"end_turn","_meta":{"jcode":{"turnId":null,"role":"none"}}}}
```

The third result is a selection-only `/model` command. Usage, when reported,
stays on the primary stream; joined requests do not repeat output or usage.

Originator detach (recorded in `04-detach-v1.jsonl`):

```json
{"jsonrpc":"2.0","id":4,"method":"_jcode/detach","params":{"sessionId":"s1"}}
{"jsonrpc":"2.0","id":3,"result":{"stopReason":"cancelled","_meta":{"jcode":{"turnId":"t1","role":"origin"}}}}
{"jsonrpc":"2.0","id":4,"result":{}}
{"jsonrpc":"2.0","id":5,"method":"_jcode/detach","params":{"sessionId":"s1"}}
{"jsonrpc":"2.0","id":5,"result":{}}
```

The other attachment sees one completed peer terminal after the provider gate
opens; departure does not emit a session-wide idle transition.

Observer detach has an already-open local bracket:

```json
{"jsonrpc":"2.0","method":"_jcode/turn","params":{"sessionId":"s1","turnId":"t1","state":"started","origin":{"kind":"client","clientInstanceId":"peer"}}}
{"jsonrpc":"2.0","id":4,"method":"_jcode/detach","params":{"sessionId":"s1"}}
{"jsonrpc":"2.0","method":"_jcode/turn","params":{"sessionId":"s1","turnId":"t1","state":"canceled"}}
{"jsonrpc":"2.0","id":4,"result":{}}
```

An inserted/adopted steer also receives a cancelled prompt result with `t1` and
`role: steer` before detach success. A never-inserted steer receives cancelled
with `turnId: null, role: none`. The local wait ended without model insertion.
Observer close uses the same canceled bracket, without stopping the originator.

No session notification arrives after detach success: tests complete the peer
turn and use initialize as a subsequent protocol barrier. No timer sleeps
establish ordering. Provider gates and debug-socket queue admission do.

## Core hook

The adapter cannot preserve execution by simply dropping its daemon socket:
owner disconnect cleanup can stop work. `PrepareDisconnect` gains optional
`preserve_work` (default false, omitted when false), advertised by the additive
`detach_preserve_work` daemon capability. Before dropping the socket the adapter
awaits its acknowledgment, which enables existing daemon continuation behavior.
Effective detach requires that capability plus attributed-turn steering/withdrawal.
The adapter withdraws its own pending inputs and joins prompt, reader and catalog
writers before answering, so local replies/updates cannot follow detach success.

## Tests

- `acp_conformance_shared_observe_steer`: origin and steer share the peer bracket ID; output and bracket terminal occur once; busy control requests remain errors without results; back-to-back Stop/prompt successor completes normally.
- `acp_conformance_create_list_load`: selection-only `/model` returns null/none; existing creation, listing and replay remain intact.
- `acp_conformance_detach_originator`: local prompt cancelled before detach response; peer work completes with one terminal; repeated detach and later notification silence.
- `acp_conformance_detach_observer`: local canceled bracket; originator completes unaffected; later notification silence.
- `acp_conformance_detach_withdraws_pending_steer`: debug-socket admission gates a never-inserted steer; detach withdraws it and settles its wait before success.
- `acp_conformance_detach_adopted_steer`: inserted steer retains turn ID and steer role when its local wait ends; originator continues.
- `acp_conformance_detach_params_and_idempotency`: malformed parameter shapes yield -32602; repeated absent-session detach returns {}.
- `acp_conformance_observer_close_bracket_contract`: enabled; canceled local bracket precedes observer close success and shared turn continues.
- Router regressions: correlation survives terminal retirement until request release; observer close and detach close brackets once without daemon cancellation; origin detach settles its prompt without daemon cancellation.
- Shape/support regression: detach needs negotiated daemon preservation and withdrawal; old PrepareDisconnect input defaults preservation to false. Existing prompt shape assertions include null/none metadata.

## Commands and results

All commands ran under `nice -n 10`, with three Cargo jobs and sufficient memory
checked using `free -m`. No full suite, coverage or watch run was used.

```sh
CARGO_NET_GIT_FETCH_WITH_CLI=true CARGO_BUILD_RUSTC_WRAPPER= nice -n 10 cargo test -j 3 --test e2e acp_conformance
JCODE_ACP_FIXTURE_RECORD=1 CARGO_NET_GIT_FETCH_WITH_CLI=true CARGO_BUILD_RUSTC_WRAPPER= nice -n 10 cargo test -j 3 --test e2e acp_conformance
CARGO_BUILD_RUSTC_WRAPPER= nice -n 10 cargo test -j 3 --test e2e acp_shared_session
CARGO_BUILD_RUSTC_WRAPPER= nice -n 10 cargo test -j 3 --lib cli::acp::tests
CARGO_BUILD_RUSTC_WRAPPER= nice -n 10 cargo test -j 3 --lib cli::acp::
CARGO_BUILD_RUSTC_WRAPPER= nice -n 10 cargo test -j 3 --lib cli::acp::daemon_router::tests
CARGO_BUILD_RUSTC_WRAPPER= nice -n 10 cargo test -j 3 --test e2e acp_conformance_observer_close_bracket_contract
CARGO_BUILD_RUSTC_WRAPPER= JCODE_ACP_FIXTURE_RECORD=1 nice -n 10 cargo test -j 3 --test e2e acp_conformance
```

Final targeted unit/router/envelope run: 100 passed. Shared-session run: 2 passed.
Final wire run: 12 passed, none ignored. Five consecutive final conformance
runs exercise every new and enabled wire scenario five times.

The busy assertion initially caused fixture drift because its legacy error
message embeds an opaque session ID and its response could race origin output.
It now runs outside the corpus, gated on observed origin text; errors stay
literal and the comparator is unchanged. Earlier assertion/initializer failures
were fixed; only successful final runs count as flakiness evidence.

Final repeat command:

```sh
for iteration in 1 2 3 4 5; do
  CARGO_BUILD_RUSTC_WRAPPER= nice -n 10 cargo test -j 3 --test e2e acp_conformance > /tmp/acp-corpus-flake-$iteration.log 2>&1 || exit 1
done
```

`~/.jcode-fork` is absent on triton, so formatting was checked with:

```sh
rustfmt --edition 2024 --check --config skip_children=true src/cli/acp.rs src/cli/acp/daemon_router.rs src/cli/acp/daemon_router_tests.rs src/cli/acp/tests.rs tests/e2e/acp_conformance.rs tests/e2e/acp_shared_session.rs crates/jcode-protocol/src/lib.rs crates/jcode-protocol/src/wire.rs crates/jcode-protocol/src/wire/events.rs crates/jcode-app-core/src/server/client_lifecycle.rs
git diff --check
```

## Contract clarifications

The original none-role examples included rejected busy prompts, but legacy
v1 returns JSON-RPC errors. The orchestrator explicitly chose preservation.
The contract now says every *successful* v1 prompt result carries correlation;
busy errors have no result and therefore no turn metadata. The wire assertion
uses a busy control prompt, which cannot implicitly steer.

The contract did not specify an identity when no turn ran. Proposed and recorded
wording: “When role is none, turnId is null.” This also covers a withdrawn input
that was never inserted. Inserted origin/steer waits retain their identity when
ended locally. Existing owner-aware close and peer notification semantics stay
in place.

## Daemon transport recovery

The adapter retries connection establishment to its captured socket for at most
10 seconds after EOF or a read error, including safe handshake I/O failures.
Each handshake attempt has a two-second deadline. Semantic rejection (missing
target, mismatched session or invalid snapshot) stops recovery. It negotiates capabilities and subscribes
to the original native session before replacing the writer. It does not launch
a daemon, discover a replacement endpoint, replay history, or resend uncertain
requests. Requests pending on the lost transport fail through their existing
error path. New requests during recovery fail rather than queueing for retry.

The logical attachment retains its client identity, skill selection, view state,
turn fence and peer mapper. A retained observed turn is reconciled against the
new daemon snapshot; retaining an identity does not mean execution resumed.
Peer brackets are not reopened for an unchanged turn. A snapshot reporting no
active turn ends a stale peer bracket with an explicit unknown-outcome error;
it does not infer cancellation or success while disconnected. A subsequent confirmed
steer can receive output from a retained turn without reviving its failed prompt.

Recovery omits Subscribe's working directory: the daemon resolves it from the
existing target, rejecting a missing target instead of using the new-session
fallback. Daemons without this target-resolution contract fail recovery safely.
Close, detach and releasing the attachment stop recovery; exhausted recovery
leaves explicit session/load available through its existing attach contract.
When the transport cannot supply its original address (currently non-Unix),
automatic recovery is disabled rather than rediscovering a different daemon.

Private Unix-socket tests in `src/cli/acp/tests/reconnect.rs` cover EOF recovery,
the next prompt, original socket capture, missing target rejection, bounded
unavailability and detach during a stalled handshake. Router tests cover peer
identity, uncertain requests, snapshot reconciliation and subsequent steering.
The opt-in `reconnect_real_private_daemon_reexec_then_fake_provider_prompt`
test in `src/cli/acp/tests/reexec.rs` also exercises a real subprocess exec with
private home/runtime/sockets and a local deterministic HTTP provider. On atlas,
the installed 1005 daemon retained PID 2819860 while its server ID changed from
`server_brook_1794185544599_6df0cb90aa047987` to
`server_bridge_1794185545127_fc9bacc2ba873cda`. This branch's ACP library retained
native session `session_snail_1794185544609_25d6d5d322294164`; the next prompt
returned `private-reexec-ok`. Exactly two provider calls occurred (seed and next).
An empty provisional session has no persisted transcript after exec and is
correctly rejected as missing; the real-exec test seeds a persisted session.
The existing Fork CI ACP step runs this opt-in test against its own built
release binary, as well as the normal ACP tests. The process proof exercises
the ACP library transport lifecycle, not the adapter's stdin/stdout framing.
No shared daemon was restarted for this change.
