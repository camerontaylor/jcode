# Jcode fork release 0.99.1005+fork.p199

Release/deployment evidence recorded on atlas, 2026-11-09. The user authorized
merging when ready and releasing/deploying after the known scoped issues were
resolved. This lane changes jcode only; Paseo releases are separate.

## Immutable source and validation

- Repository: `camerontaylor/jcode`.
- Annotated tag: `v0.99.1005+fork.p199`.
- Tag object: `c5060040aaf219a7cd657795df5ee1b5563c7095`.
- Peeled tagged/custom source: `d66d8d2181069c3524345723cf8ac0772b23944f`.
- Upstream: `v1.0.0-preview.199`, `7fd42339c907de223021bd63e5bb5df778c43a01`.
- Exact-head Fork CI: [42534258327](https://github.com/camerontaylor/jcode/actions/runs/42534258327), success.
  ACP adapter unit tests: 128 passed. E2E: 97 passed, 7 ignored. The actual
  Stop/bash-wait, repeated-active-load and transport-loss/waiting-steer
  regressions are enabled and passed. All fork quality guards passed.
- Release workflow: [42536770694](https://github.com/camerontaylor/jcode/actions/runs/42536770694).
  Success: provenance preflight, all four native builds and publication passed.
  Native packaging ran in CI only.

Scope includes the reviewed cache/reexec lanes, Stop cancellation retention,
repeated-load attachment reuse, transport-owned queued-input withdrawal, and
contract regressions. Peer configuration remains intentionally serialized.
Attachment identity stays stable only while its live connection is retained;
reconnection, other sessions and other processes retain distinct identities.
Review details: [ACP_INTEGRATION_REVIEW.md](ACP_INTEGRATION_REVIEW.md).

## Deployment baseline

All seven hosts began on `0.99.1004+fork.p199` (`0f9a3e5de`) with current,
stable and shared-server channels tracking the same release. Linux glibc:
atlas/triton/uranus 2.43, pluto 2.40, ceres 2.44, all above the native release
baseline 2.39. Saturn is macOS arm64; Neptune is macOS x86_64.

The existing listeners were Triton `/tmp/jcode-502/jcode.sock` (PID 1892446,
five connected session IDs) and Neptune's per-user `runtime-ctaylor/jcode.sock`
(PID 87836, one connected session ID). Their daemon executables were verified
as release 1004. No remote Paseo jcode-provider turn was running at initial inventory.
A later Triton turn became active before installation; see the incident below.
General debug controls were disabled and were left unchanged; read-only Ping
and process executable inspection supplied the baseline. Idle client/session
IDs are compared across reload; no user conversations are used for smoke.

Approved deployment method: the agents harness updater, `--only jcode`, using
cached `harness-update-f70952f.py` on checkouts with unrelated edits and the
explicit manifest `~/.local/agents/manifests/harnesses.conf`. Atlas may use the
same updater because it now reports a release version; the documented manual
SHA256-verified launcher-plus-binary installation remains a fallback if its
development stamp prevents updating. Existing channel links and marker files
are backed up before installation. No shared daemon is hard-stopped.

## Publication and rollout results

Published Latest, non-draft/non-prerelease:
[v0.99.1005+fork.p199](https://github.com/camerontaylor/jcode/releases/tag/v0.99.1005%2Bfork.p199).
All four downloaded archives matched their published SHA256 files and expected
archive layout. Published provenance identifies exact tagged source `d66d8d218`.

Native binary SHA256 (Linux values refer to the ELF sibling of the launcher):

| Platform | SHA256 |
| --- | --- |
| Linux x86_64 | `41471ead14514853b9b7522e8cfd0c6b4d80ebf9743a114e41d7d75c6fb65220` |
| Linux aarch64 | `ef32e7978550576d75568ad686061be030996e0448502078208dcb69ac7c9a29` |
| macOS aarch64 | `ba4cccf5a0973737a6f7b21273659fe350d7f199a1b458bf794569c4e2401cbf` |
| macOS x86_64 | `646d3db7597b8382e3820d77ead6aedb34472faedd0b86992382659946aa9f83` |

All seven hosts have verified immutable version
1005, exact asset member hashes, current/stable/shared-server channels and
markers, and post-install capability probe exit 73. Each host's installed member
hashes were compared directly with the verified published archive members.

| Host | Private disposable session | Actual shared socket |
| --- | --- | --- |
| Atlas | 13/13 passed | 13/13 passed; explicitly started for smoke, not preexisting |
| Triton | 13/13 passed | 13/13 passed; preexisting daemon PID 1892446 |
| Uranus | 13/13 passed | No live socket at final inventory; no preexisting-live claim |
| Saturn | 10 protocol checks passed; prompt blocked by baseline OpenRouter 401 | No live socket at final inventory |
| Pluto | 10 protocol checks passed; prompt blocked by baseline OpenRouter 401 | No live socket at final inventory |
| Ceres | 10 protocol checks passed; prompt blocked by baseline credentials/Copilot 403 | No live socket at final inventory |
| Neptune | 13/13 passed | 13/13 passed; original daemon PID 87836 and socket inode unchanged |

An initial report incorrectly labeled a second private smoke on Saturn, Pluto,
Ceres, Uranus and Neptune as live: those hosts' cached helper did not implement
`--existing-socket` and silently created a private daemon. Those live labels are
retracted. The private results remain valid. A reviewed helper with explicit
existing-socket support was copied to a release-specific cache path; only
Neptune had a live shared socket at the final check and is being retested there.
No shared daemon was stopped by the older helper, which targeted its own
private socket. Credentials were not changed.

Neptune update and targeted recovery succeeded. Its daemon retained PID 87836
and moved to the verified 1005 executable on the same per-user runtime socket.
Before both updater and supported CLI agent reload, agent
`4d3df6a4-e419-4f2c-b2e6-071e9f97465d` was idle with no active turn or pending
permissions. `paseo agent reload <id> --json` returned `reloaded`, timelineSize 149.
The same native session `session_bonehound_1792889581862_af4fea67bf175815` appeared
in socket Ping and remained idle afterward. Its 188-message, 1,433,380-byte
conversation remained byte-identical, SHA256
`1d05020f4474e2b218dd64c64afa75073225d0f7a9a34cd6697f10f2dece7c84`.
No recovery marker existed before or after; no user prompt was submitted.
Neptune private and corrected existing-socket smoke passed all 13 checks.
PID 87836, process command and socket inode 69898860 were identical before and
after that smoke. The 188-message native conversation hash remained identical.
However, subsequent Ping timed out; see the unresolved health issue below.

## Triton reload incident and disposition

The deployment operator launched the updater in the same tool batch as a fresh
active-agent query, before inspecting that query. This ordering mistake allowed
reload during one active turn. The old 1004 daemon notified six clients and
checkpointed the one generating session at `2026-11-09T00:26:28.809Z`. It logged
stream cancellation for graceful shutdown, processing completion and checkpoint
completion before exec. The daemon retained PID 1892446 and the same socket
`/tmp/jcode-502/jcode.sock`; 1005 published socket readiness at 00:26:28.973Z.

Five old ACP attachments disconnected. Agent `9d79d3e5-83bd-4113-abd8-64a684090d97`
subsequently reported `Broken pipe (os error 32)` when its coordinator attempted
new review work. Its previous review was complete; its owner confirmed no retry
of that old agent was needed and started replacement reviewer
`f49aaec5-4d5b-456e-8ca8-bc6bb1e5c95b` on the installed 1005 daemon. No deployment
operator prompt was sent to either reviewer, and no further Triton reload was
performed. Replacement completion was confirmed by its coordinator: actual finished event
`bb9d9279-3135-4287-98c1-eb278f038d62`, with a completed independent review at
[PR1279 PASS](https://github.com/camerontaylor/cq-toolkit/pull/1279#issuecomment-6729624604).
The coordinator confirmed no further review work was pending at that point.

The remaining four agents were idle with no active turn:
`8cd4f205` / session_dove (798 persisted messages), `adf9fe3f` / session_fish
(340), `9f3778d0` / session_bird (101), and `933367b7` / session_kikazaru (338).
Their native handles and conversation files remained present. Their owning
coordinators were notified; no review work was fabricated to exercise reconnect.
The interrupted session_bug retained its 105-message JSON, append journal and
pending interrupted-peer recovery intent. Logs, trace, recovery intent and
conversation snapshot were preserved under Triton
`~/.cache/jcode-release-1005/incident/`.

This is an existing ACP reconnect limitation, not evidence of a 1005 regression:
the 1004 adapter reader in `src/cli/acp/session.rs:545` ends on daemon EOF and
closes its router without reconnecting. That reader is unchanged in 1005.
Reload was executed by the old daemon on the unchanged socket; an old adapter's
next write can encounter a broken pipe. A fresh `session/load` or supported
Paseo agent reload can attach the persisted native session. Transparent adapter
reconnection remains a follow-up issue; no protocol redesign or speculative
rollback was included in this release.


## Neptune post-smoke health issue (rollout health remains incomplete)

After the disposable shared-session prompt completed and closed, new main and
debug socket Ping requests timed out. Recorded request bytes contain valid JSON
and a newline. A valid debug `state` request responded immediately with one
session and `is_processing=false`; malformed debug input also returned an error
immediately, excluding a global event-loop stall.

A read-only one-second macOS process sample identifies the blocked chain:
`cleanup_client_connection` -> `Agent::mark_closed_with_reason` ->
`telemetry::end_session_with_reason` -> `emit_lifecycle_event` ->
`detect_project_profile` -> `walkdir` -> `read_dir` -> `open_nocancel`.
The daemon process working directory is `/Users/ctaylor`. Disconnect cleanup
holds the global `client_connections` write guard across this synchronous
telemetry filesystem scan. Both Ping paths wait for that registry's read lock.

The causal cleanup/Agent/telemetry source is identical in 1004 and 1005.
Ordinary isolated post-close Ping probes passed for both versions on atlas;
the exceptional slow filesystem path needs a deterministic gated regression.
Evidence is retained on Neptune under `~/.cache/jcode-release-1005/`, including
`post-smoke-sample.txt`, `post-smoke-wire.json` and `live-target-after.json`.
No diagnostic daemon restart, user prompt or filesystem deletion was performed.
The same process and persisted conversation were retained while a scoped fix
was developed on `fork/acp-disconnect-telemetry`. Fleet installation is complete;
fleet health and controlled Neptune recovery remain pending this fix and review.
