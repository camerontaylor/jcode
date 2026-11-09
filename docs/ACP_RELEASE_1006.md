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
is running the four native builds. The provenance preflight has passed.
No 1006 host installation has been performed at the time of this update.

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
