# Live v1 acceptance harness

Durable, reproducible live evidence for the ACP v1 contract
(`docs/ACP_FORK_INTERFACE.md`) and strategy Phase 3. It does **not** replay
scripted ACP fixtures: a real `jcode` binary serves ACP, and the real Paseo ACP
connector drives it.

## What runs

| Artifact | Path | Proves |
| --- | --- | --- |
| Connector harness | `jcode-live-acceptance.ts` | Real Paseo daemon + `plugins/jcode-provider` + real `jcode acp` child: import/replay, forward steer, reverse steer, single-settlement turn correlation (concurrent steer into one held execution, one terminal, one usage total), Stop then immediate prompt, Stop of an observed peer turn, model/effort propagation both directions, textual and explicit skill selection with `null` clear, owner-aware close, observer detach (peer work survives, updates stop, session reloads), and explicit detach that preserves inserted work. |
| ACP companion probe | `jcode-acp-probe.ts` | Real `jcode acp` process: `interfaceVersion`/extensions/`sharedControl` (all four booleans), prompt correlation roles (`origin`/`steer`/`none`), explicit `_meta.jcode.skill` select and `null` clear, `_jcode/detach` semantics and `-32602`, config options and legacy aliases. |
| Barrier regression | `jcode-live-barriers.ts` | Deterministic proof of the harness's causal barriers through the real daemon: a raw-attachment terminal withheld past the consumer's settle is still waited for; a held origin model call cannot be released until the daemon has admitted the steer (`pending_soft_interrupts` plus, for a peer steer, its `ack` id). Delivery is gated, never slept or retried. |
| Runner | `run-live-acceptance.sh` | Provenance gate, isolated environment, evidence directory. |

Each harness runs its own `jcode serve` on a private socket with private
`JCODE_HOME`, `JCODE_RUNTIME_DIR` and (for the connector) `PASEO_HOME`. They
never talk to the shared daemon and never change user config. The model backend
is a local OpenAI-compatible HTTP server with deliberately released responses,
so turns are deterministic and no provider tokens are spent.

The connector harness imports the consumer modules from
`PASEO_CONSUMER_ROOT` by absolute path. The runner accepts an absolute or
relative value, defaults it to the sibling checkout `../paseo-v1-acceptance`
next to this repository, and resolves it to the physical checkout before any
harness, `createRequire` or evidence path uses it. Set it explicitly when the
consumer lives elsewhere. It never copies, stubs, or edits consumer source.

The runner starts Node with `--conditions=source`, so `@getpaseo/plugin`
resolves to the checkout's TypeScript source rather than a possibly stale
`dist`. The evidence records the resolved `pluginEntry` and the consumer
`head`, `tree` and worktree digest, so the exact code under test is auditable.
When the consumer has not built `dist`, this is also the only way to exercise
the edits under review.

## Run

Run from a checkout with a current binary and a sibling consumer checkout:

```bash
JCODE_E2E_BINARY="$PWD/target/debug/jcode" \
JCODE_EXPECTED_PROVENANCE=<git-sha-in---version> \
  scripts/live-acceptance/run-live-acceptance.sh
```

Or point at the consumer explicitly:

```bash
PASEO_CONSUMER_ROOT=/path/to/paseo \
JCODE_E2E_BINARY=/abs/path/to/jcode \
  scripts/live-acceptance/run-live-acceptance.sh
```

Outputs go to `JCODE_ACCEPTANCE_OUT_DIR` when set, otherwise to
`JCODE_SCRATCH_DIR/live-acceptance/live-run-<utc>` when `JCODE_SCRATCH_DIR` is
set, otherwise to `<repo>/target/live-acceptance/live-run-<utc>`:

- `provenance.txt` - binary version, sha256, consumer head/branch/dirt, node
- `connector.evidence.json`, `connector.log`
- `probe.evidence.json`, `probe.log`
- `barriers.evidence.json`, `barriers.log`
- `summary.txt`

Each evidence file has `provenance`, `environment.isolated`, and a `scenarios`
object with `pass`/`fail` and the observed wire/state detail. The scripted
model can attach an OpenAI streaming `usage` chunk to a reply; `s04` uses
distinct per-call counters to prove the connector reports one execution total
instead of dropping or double-counting a call. A failed scenario is also
printed to the job log immediately, and the runner prints the same shared
bounded failure lines from the evidence file if the harness exits nonzero, so
a discarded evidence directory cannot hide which scenario failed or why.

The connector's steering scenarios hold the origin model call until
`pending_soft_interrupts` (and the peer `ack`) prove daemon admission, then
wait for the exact turn's scoped `soft_interrupt_injected` and terminal. They
never treat consumer idle or a timeline echo as admission, and they never
sample the raw attachment once after `waitIdle`.

Useful debug knobs for the connector harness:

- `JCODE_ACCEPTANCE_SCENARIOS=s04,s09` runs only matching scenario names or
  prefixes; the rest are recorded as `skip`.
- `JCODE_ACCEPTANCE_KEEP=1` keeps the isolated workspace and prints its path
  instead of deleting it.
- `JCODE_ACCEPTANCE_ONLY=connector|probe|barriers` runs a single harness.

## Honest scope limits

- The probe is live binary evidence but **not** Paseo-connector proof. The
  report labels it as a companion.
- `sharedControl.observe` has no separate consumer switch; it is exercised by a
  Paseo attachment observing a peer-origin turn, which is what the boolean
  gates.
- A fresh `fetchAgentTimeline` for a closed record can load provider history
  again. `s12` therefore proves "updates stop" on the push channel
  (`agent_stream` messages after detach), not by re-reading the timeline.
- A stale `JCODE_E2E_BINARY` (for example the 2026-11-09 `900265ac7` build)
  produces smoke evidence only. The runner warns when it sees that version.
