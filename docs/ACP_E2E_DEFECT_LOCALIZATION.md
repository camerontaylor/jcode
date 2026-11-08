# Shared-session E2E defect localization

Investigation on atlas, 2026-11-09 Melbourne. Adapter/daemon baseline:
`0f9a3e5de`, installed `0.99.1004+fork.p199`. Paseo source inspected read-only:
`origin/custom` at `324dff30cdf25155a139edc69ba1938d5411f5e9`, and deployed
`6b45103d7`. Starting report: `~/.cache/paseo-jcode-e2e/REPORT.md`.

## Isolation and method

A private daemon ran with `JCODE_RUNTIME_DIR=/tmp/acpfix-*`, an explicit
socket, and OpenAI / gpt-6.1-sol. A raw JSON daemon attachment (subscribed
with turn identity support) stood in for the TUI. The installed adapter was
controlled by a raw stdio ACP client. No Paseo daemon or shared jcode daemon
was operated on. The probe and full captures are retained on atlas under
`~/.cache/acpfix-evidence/`; full captures are not checked in because they
include unrelated account/model metadata. A socket relay records the actual
adapter-to-daemon requests as well as the ACP traffic.

## Stop

A direct run stopped the raw peer turn in about 9 ms, but a repeated run
with an adapter socket relay reproduced the failure. The actual daemon request
was `Cancel { id: 2876856437315731458, turn: Some(TurnIdentity {
runtime_generation: "88f74d1b-701a-4f98-8a06-d2dd27211c6b", turn_id: 2 }) }`.
That exactly matched the raw attachment's `turn_started.identity`. The daemon
broadcast `turn_stopped` for turn 2 at t+28.02 s. At t+28.79 s the same turn
reported bash moved to background "by the user", ran Bg at t+31.98 s, and
finished that wait at t+50.08 s without a cancelled terminal. This is a jcode
core defect, not a missing ACP cancel dispatch.

`src/cli/acp.rs:439` withdraws waiting prompts, then reads the running turn
identity at :450. `src/cli/acp/daemon_router/lifecycle.rs:5` calls `withdraw`;
it does not clear `peer_turn`. The proposed lost-identity explanation is false.

Root cause: the no-local-task branch of
`crates/jcode-app-core/src/server/client_processing.rs` resets the control
signal after 500 ms even when it is the active agent's own shutdown signal.
`crates/jcode-app-core/src/agent/turn_streaming_mpsc.rs:1626` waits up to
750 ms for bash to complete after shutdown. Its subsequent shutdown check
at :1712 can then observe a cleared signal and take the ordinary background
handoff path, continuing the turn. Timing determines whether Stop works.

The thin core fix keeps the signal armed for identity-aware runtime turns.
Their next turn claim already clears old cancellation under the turn-state
lock before publishing identity (`discard_cancel_armed_before_turn`). The
legacy path retains its existing epoch-guarded deferred reset. Regression:
`peer_cancel_stays_armed_until_turn_retirement`, which retains a peer-owned
lease across the 800 ms window and then checks a successor starts cleanly.

Paseo source pointers (both deployed and inspected current source):
`packages/plugin/src/server/acp-internal/connection.ts:895` (`interrupt`)
invokes :922 (`interruptTurn`), which sends cancel for `nativeSessionId`.
At :930 (`scheduleVendorTurnCancel`), a grace timer locally ends the vendor
turn even without a daemon terminal. Separately,
`packages/server/src/server/agent/agent-manager.ts:3536`
(`cancelAgentRunNow`) can force-settle its manager-owned run on timeout.
These explain how idle can be displayed without proving the daemon stopped;
they do not prove which dispatch/session error occurred in the original run.

## Reverse steer

The raw peer injected `Reply REVERSE-STEER instead` into an ACP-owned turn.
The daemon emitted scoped `soft_interrupt_injected`; ACP emitted
`session/update` with `sessionUpdate: user_message_chunk`, the exact steer
text, and a message ID ending `:own:<request>:steer:0`. Assistant output
subsequently said REVERSE-STEER. This defect localizes to Paseo consumption.

The adapter already delivers this event through
`src/cli/acp/daemon_router/lifecycle.rs:325` (`on_injected`) and renders it in
`src/cli/acp/prompt.rs:218`. Existing regression:
`peer_steers_are_mirrored_as_user_messages` covers both directions.

Paseo's `packages/plugin/src/server/acp-internal/connection.ts:1299`
(`reduceLiveUserChunk`) drops user chunks when `activePrompt` is non-null
and there is no vendor turn. That is exactly this case. The Paseo change
should render externally supplied steer chunks during its own turn while
suppressing only identified echoes of its own submissions. Test an active
foreground prompt, a peer steer with its own messageId, and the resulting
single user timeline item; retain own-prompt echo suppression tests.

## Duplicate imported first prompt

After one completed peer prompt and answer, raw ACP `session/load` emitted
one user chunk for `Reply exactly ACPFIX-FIRST`, with stable ID
`<session>:msg:1`, followed by one assistant message (`:msg:2`). It did not
replay the user twice. Existing adapter regressions include
`history_replay_gives_every_message_a_distinct_stable_id`,
`replay_ids_prefer_the_stored_message_index`, and
`a_replayed_prompt_is_dropped_from_the_held_announcement`.

The original duplicate is downstream of this clean load reproduction;
a precise Paseo root cause remains unconfirmed. Trace identities through
`packages/plugin/src/server/acp-internal/connection.ts:1352`
(`reduceUserMessageChunk`),
`packages/server/src/server/agent/plugin-provider.ts:1063` (`importSession`)
and :1454 (`timelineHistory`), and
`packages/server/src/server/agent/agent-manager.ts:1728` (import timeline
seeding). Import already uses `historyPrimed: true`; do not assume it
unconditionally replays history again. The required Paseo regression should
import a one-prompt session and assert one user item through registration,
durable persistence and readback, retaining its message identity. Do not
deduplicate arbitrary equal text: repeated legitimate prompts are valid.

## Disposition

Stop has a confirmed jcode core timing cause and a scoped core fix. Reverse
steer has a confirmed consumer-side cause. Duplicate requires additional
Paseo-side capture before naming a definitive cause or choosing a fix.
The private raw-client reproduction is narrower than a full Paseo/TUI rerun.

## Verification

The new core regression failed before the fix with “a still-active turn must
retain its Stop signal”, and passed after it. The targeted
`server::client_lifecycle::turn_event_channel_tests` module passed all 15
tests, including scoped cancellation, successor isolation, and fanout ordering.
The adapter regression `stop_preserves_observed_turn_until_daemon_terminal`
checks Stop retains the exact peer identity until the daemon terminal arrives.

The fork formatter and module, code-size, test-size, panic, swallowed-error,
dependency-boundary and wildcard-reexport checks passed with the fork budget
overrides. No budgets were raised. Full raw traffic and local verification
logs remain in the atlas evidence directory named above.
