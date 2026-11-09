# Jcode 1006 release evidence

The authorized follow-up release is `v0.99.1006+fork.p199`, tagged at exact
`043e1f73850d1575882255a311e962f219fbd486`. The annotated tag object is
`50c875008708c700500bd5a57fab2d6938247b77`. Remote `custom` was verified at
that source before tagging. The release excludes these evidence-only commits.
It retains upstream preview.199 base `7fd42339c907de223021bd63e5bb5df778c43a01`.

Combined [Fork CI 42544404044](https://github.com/camerontaylor/jcode/actions/runs/42544404044)
passed both jobs on the exact source. The explicit cleanup/Ping and deferred
installation-ID snapshot regressions each ran and passed. Normal ACP tests
passed 136; the explicit private real-exec test passed one with zero ignored,
using CI's own release binary; E2E passed 97 with seven ignored. Formatting,
all-target/all-feature clippy with warnings denied and all fork ratchets passed.
Full integration details and the prior incident corrections are retained in
[the 1005 record](ACP_RELEASE_1005.md).

This release contains two scoped fixes: owned terminal reporting moves beyond
the global presence guard onto a blocking worker, and ACP adapters reconnect
to the same native session after daemon EOF with bounded, safe handshakes.
Uncertain requests are not replayed. A blocked telemetry filesystem operation
can still occupy its worker; durable session persistence remains synchronous.

[Release packaging 42545917497](https://github.com/camerontaylor/jcode/actions/runs/42545917497)
passed provenance, all four native builds and publication. The release was
published at `2026-11-09T02:01:15Z`, non-draft and non-prerelease, as Latest.
All four archives match both published `SHA256SUMS` and GitHub asset digests;
their members match the expected executable-only archive layout. All four CI
version probes report `v0.99.1006+fork.p199 (043e1f738)`. The privately tested
ARM Linux/Mac CI archives are byte-identical to the published assets.

| Asset | Archive SHA256 | Native executable SHA256 |
| --- | --- | --- |
| Linux x86_64 | `191619e52f7c6d6635e43c67e4cc195539efde16e6beaa34af98e63e9279e912` | `12f80dc0dbf304abe6b1a97c278067fbe1f915047941af9fa7c6f4842c008aff` |
| Linux ARM | `b16b648a9aca829fc123fd3d8145ee3940e45272343657f591829dfb3730538c` | `8aa96e209981f7744fc316b0d1fa107c196c2fcdb42d4274a7cf436bd5ad833e` |
| macOS ARM | `259b3060d1082bb07503cffaa1161f307dea1dad37cfa61abc55e223b6113012` | `4d3a97650211724e9f32263e8f3f667b5a7e7ded04af068dd4b2e8e7574d92e8` |
| macOS Intel | `2d23f17b1d7f11dbd31f1539cf30a47ad1d4b9d1f5d8af5a58008ab3a348e08f` | `2fb16d1e3771f2464266ae7fd06bc069e82860b41fba10f6c90e7c528f9c370d` |

Linux launcher hashes remain `b7a0775811409a2959f9dd2ad0d72a03e9caac7a8495f8e6aee48420839b3c23`
(x86_64) and `2e091ad7ad1f6a390d5433a8c64f287b72d4bad1dc616e725aedd8fdf06ce78d`
(ARM). The verified x86_64 archive/native hashes and release URLs were sent
directly to the Paseo owner and Ceres consumer validator. Their 39fb source
contract gate remains unchanged; separate released-1006 consumer validation is
owned by that lane.

## Installed-binary smoke provenance

The release-specific private smoke at atlas
`~/.cache/jcode-release-1006/installed-reexec-smoke.py` runs both an actual
installed ACP subprocess and a daemon against a private home, socket and fake
OpenAI-compatible provider. It seeds a persisted native session, requests a
real exec, checks daemon PID/native session/attachment identity, sends the next
prompt exactly once, closes its disposable session and checks bounded Ping.
It uses no live credentials or user conversation. A negative control against
installed 1005 fails at the expected missing-adapter-reattachment assertion
after the real exec. Its private processes are cleaned up on failure.

Initial read-only inventory found preexisting shared attachments only on
Triton (whale) and Neptune (bonehound). Other hosts had no responsive shared
daemon. Both retained Paseo agents were idle; fresh status and permission
checks will be inspected immediately before each updater and targeted reload.
Old 1005 ACP processes require a supported idle Paseo reload to adopt 1006;
the new binary alone does not upgrade an already-running adapter process.
The Triton owner confirmed its review work was complete and no new review
work was pending. No user prompt will be used to validate those conversations.

Baseline native snapshots: whale has 156 messages, 420,224 bytes, file SHA256
`4880d4d40717f425042b32b67ae46728d55a87b2d388a8fcc012369b30e43612`;
bonehound has 188 messages, 1,433,380 bytes, file SHA256
`1d05020f4474e2b218dd64c64afa75073225d0f7a9a34cd6697f10f2dece7c84`.
Neither has a reload-recovery record. Fresh before/after snapshots will be
compared during rollout rather than assuming these initial snapshots stayed
unchanged while packaging ran.

Before publication, the completed Linux ARM CI artifact passed this actual
ACP/daemon process smoke on atlas; the macOS ARM CI artifact also passed on
Saturn. Both report `jcode v0.99.1006+fork.p199 (043e1f738)`, retain the original
daemon PID and native session/attachment across exec, make exactly two fake
provider calls, and answer post-close Ping. These are private CI-artifact
checks, not claims of installed fleet validation. Logs are retained in the
1006 release cache. Short `/tmp/jcode-1006-*` socket paths keep the same smoke
portable to macOS Unix-socket limits.

## Rollout status

All seven hosts—atlas, triton, neptune, saturn, pluto, ceres and uranus—are
installed at exact 1006.
Installed member hashes, immutable layout, current/stable/shared-server links,
version markers and hidden post-install probe exit 73 match the release.
Each passes the actual installed ACP/daemon private re-exec smoke with the
same native session and attachment identity, exactly two fake-provider calls
and responsive post-close Ping. Backups of prior links/markers are retained at
`~/.jcode/builds/state-backup-release-0.99.1006+fork.p199/` on updated hosts.
The pinned reviewed updater SHA256 is
`5717c7cac7b4190756f98d2f1b034adba7617ffbb0699f25a0c0904c3123ec4b`.

The first Uranus private smoke raced readiness: native Ping presence appears
before the ACP reader has finished draining Subscribe's snapshot. The next
prompt was rejected before transport write, with no retry. The corrected
harness waits for a successful read-only history round trip on the same
attachment before sending its next prompt exactly once. It retries only that
read-only readiness probe, bounded at ten seconds. Uranus then passed; the
corrected harness also passed on the other updated hosts. The original failed
log is retained. This changed the test's readiness observation, not production
source or cancellation semantics.

Atlas, Pluto, Ceres, Uranus and Saturn had no responsive preexisting shared
daemon; their evidence is explicitly private. No shared daemon was started to
manufacture a live-validation claim. Neptune's actual preexisting shared
daemon retained PID 87836 across exec, now reports 1006, and uses the original
runtime socket with new inode 69910980. Bonehound was inspected idle with no
active turn or permissions immediately before the updater and again before
supported Paseo reload. Reload kept the same agent/native handle and 149-item
timeline; its replacement ACP PID 65915 reports 1006. The exact 188-message,
1,433,380-byte JSON snapshot hash remains unchanged, with no recovery record.

Neptune's explicit existing-socket disposable smoke passed all 13 checks.
PID 87836, inode 69910980 and process command matched before/after; a later
bounded Ping lists only bonehound after the disposable attachment retired.
Bonehound remains idle with no active turn or permissions. No user prompt,
credential change or Paseo daemon restart was performed.

Triton was deferred when the immediate pre-updater check at
`2026-11-09T02:07:10Z` found six new critics in addition to whale, four running.
No updater or reload ran then. Owner 80eff5db held new dispatches and notified
completion of the last turn; its error was the separately reported HTTP 429
`GoUsageLimitError` monthly provider quota. Fresh checks at 02:28:07–08Z showed
all seven without an active turn or pending permissions (six idle, one ended
in quota error). Only then did the updater run. PID 1892446 exec'd into the
verified 1006 native payload. A second fresh inspection preceded seven
supported Paseo reloads, which all succeeded. Their replacement adapter PIDs
2659784, 2659904, 2659955, 2660014, 2660078, 2660132 and 2660185 report 1006.

Every native handle and complete materialized history digest matches exactly
before/after. Original snapshot bytes and original journal prefixes remain
identical; each journal appended four metadata-only records. Snapshot and
journal copies remain on Triton under
`~/.cache/jcode-release-1006/session-snapshots-before/`.

| Native session | Materialized messages | Canonical history SHA256 |
| --- | ---: | --- |
| lizard | 168 | `5bf3c0e1a5fa42b67054cf8566e6111e3b75b7416936715bf321401186bea924` |
| camel | 126 | `dc5f9e2b88633df2040a473dbef32b041767137783fa650790af39b7518da9f1` |
| hippo | 82 | `ed1894c0f88b0eba07ccf67348bd3e0cd585e72e2e09d0f5a7f917d0835553b1` |
| whale | 201 | `343791a7b843fe3720b1736edf154a36ffe07c55cd113daef428c3b29d50f548` |
| flamingo | 107 | `1bd318ab8e1bf47bf0df18c5970abaa330b5fe0daf09505b22bbd6faba417a03` |
| skunk | 64 | `d79b3b85277726bade9d921e18dab8401f5833ff25fbd721486e4ba1c1a7a0c3` |
| octopus | 145 | `32b375cb9a22d03a19b0e66332f7d7b546c4493d43c9f881e7c5c5513faffc76` |

Triton's installed private fake-provider exec smoke passed. Its actual shared
socket `/tmp/jcode-502/jcode.sock` passed all ten protocol-only checks with
PID 1892446 and inode 40493503 unchanged across that smoke. No real provider
prompt or quota retry was attempted. A later Ping lists exactly the seven
preserved native handles; all seven agents are idle without an active turn or
permissions. The coordinator's provider/review hold is independent of this
completed maintenance window. No review was cancelled, repeated or fabricated.

Three legacy unattached ACP processes remain associated with historical
errored agents 40fd5313, 382f0ad7 and 9d79d3e5 (PIDs 1896487, 2106455 and
2149617; binaries 1003/1003/1004). Each has no active turn or permissions and
none appears among the seven shared attachments. They were not killed or
resumed. An owner reusing those old errored processes still needs supported
reload; installation cannot retrofit their running executable. The seven
targeted current views are the seven verified new 1006 adapter processes.

An independent reviewer later started a new atlas shared daemon after its
installation; read-only inspection identified PID 2904645 running 1006 and its
native cat attachment. It was not reloaded or used for a deployment smoke.
The review's later provider-quota error was reported by its owner separately.

## History-count correction and evidence limits

Earlier whale 156 and bonehound 188 counts referred only to JSON snapshots.
Jcode also persists append journals: whale's complete pre/post history is 201
messages, and bonehound's current materialized history is 226. Bonehound's
38 journal messages all carry October 25 timestamps; the 1006 reload appended
metadata only. Its current canonical history digest is
`07f344eb534b67bbf04dcb5f32ef292bb67331d9d5be5e5f8d44a4c8d55e3f57`.
Neptune's pre-update journal hash was not captured, so this report does not
claim a historical journal byte-for-byte comparison there. Its exact snapshot
hash and 149-item loaded timeline are the independently compared evidence.
Triton's seven comparisons include snapshots plus complete journals captured
before exec. The linked 1005 record now explicitly labels its snapshot counts;
its prior smoke-label corrections and operator ordering mistake remain intact.

All updater/reload windows are closed. No Paseo daemon restart, credential or
billing change was performed. All private smoke processes were reaped; their
temporary homes and sockets were removed. The separate OpenCode Go monthly
quota incident remains outside this jcode release and was not retried.

## Separate consumer-gate follow-up

The later Ceres gate for exact Paseo candidate `2b9ab453` is **not green**:
the private verified-1006 scenario passed its one test with zero skips, but
Vitest reported one uncaught `ECONNREFUSED` on its private jcode socket and
exited 1. The corrected 39fb contract scenario and preceding static/named
gates had passed. Failure evidence is preserved on Ceres under
`~/.paseo-fork/state/pr13-final-gates/daemon1006.log` and `.exit`.

Read-only inspection identified an unhandled readline error path in that
fixture's relay: socket error handlers exist, but its readline interfaces
forward the same errors without interface error listeners. Teardown kills
the native daemon before closing Paseo adapters and the relay, exposing that
path to 1006's reconnect attempts. The root assigned a sole Paseo fixture
owner to establish bounded proof and orderly shutdown/error propagation.
The release lane performed no rerun, instrumentation, source fix or deployment
in response. Its completed fleet runtime evidence above remains distinct from
this failed consumer gate.
