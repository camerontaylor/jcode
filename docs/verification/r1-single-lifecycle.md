# R1 single lifecycle verification

Status: inventory; lifecycle replacement and equivalence gate are pending.

Baseline: `47a130e98`, branch `fork/r1-single-lifecycle`, host triton.
Memory at start: 18,965 MiB available. Configured contributor identity retained.

## Replacement map

- `server/state.rs` is the only production publisher of daemon `TurnEnded`.
  `TurnEventDrain::forward` classifies producer Error, cancellation epoch, or
  completion, retires active attribution, then publishes end and scoped terminal.
  `close` classifies panic as failed and producer drop as cancelled. Replace the
  separate end with authoritative information on the scoped terminal. Preserve
  queue drain, publication pin, terminal deduplication, and snapshot retirement.
- `agent/turn_ownership.rs` supplies cancellation epoch probes and retirement.
  Their information and atomic snapshot behavior remain necessary; deleting
  them requires a replacement tied to terminal publication, not Stop intent.
- Protocol enum, reexports and serialization tests encode the competing event.
  Replace with a minimal generic terminal refinement; omit optional fields for
  old producers. Completed/failed/cancelled must remain authoritative.
- `client_lifecycle.rs` strips identity events for legacy readers. Removing the
  variant removes D3, rather than extending capability filtering for it.
- ACP router consumes end events for own cancellation, adopted brackets and
  peer retirement. Its scoped Done/Error fallback already closes brackets but
  cannot distinguish cancelled Done. Move classification to that path before
  deleting the event. Locally synthesized ends used for close/detach need an
  adapter-owned presentation channel, not a daemon ServerEvent variant.
- `acp.rs` renders adopted ends on the prompt stream. Preserve output ordering
  and bracket-before-response ordering through an adapter-owned delivery type.
- Harness translation ignores the attribution event; delete its two match
  entries with the variant. TUI readers and event classification consumers must
  remove obsolete match arms. Existing tests must retain their ordering,
  snapshot, panic, cancellation and publication reservation assertions on the
  surviving scoped terminal.

## Complete symbol inventory (including tests)

Paths and lines refer to the baseline, before the router move.

```text
src/cli/acp.rs:3:use crate::protocol::{Request, ServerEvent, TurnEndStatus, TurnOrigin};
src/cli/acp.rs:285:            // `PeerTurnStarted`/`PeerTurnEnded` would leave the client with an
src/cli/acp.rs:1801:                ServerEvent::TurnEnded {
src/cli/acp.rs:2196:            RouteAction::PeerTurnEnded {
src/cli/acp.rs:2461:fn turn_end_state(status: TurnEndStatus) -> &'static str {
src/cli/acp.rs:2463:        TurnEndStatus::Completed => "completed",
src/cli/acp.rs:2464:        TurnEndStatus::Failed => "failed",
src/cli/acp.rs:2465:        TurnEndStatus::Cancelled => "canceled",
src/cli/commands_tests.rs:898:            take_run_gate_digest_if_turn_ended(session, false, &open, true),
src/cli/commands_tests.rs:921:        take_run_gate_digest_if_turn_ended(session, false, &done, true),
src/cli/commands_tests.rs:984:    assert!(take_run_gate_digest_if_turn_ended(session, false, &[], false).is_none());
src/cli/commands.rs:2840:    run_external_blocker_notice, run_todos, take_run_gate_digest_if_turn_ended,
src/cli/commands.rs:2890:        let gate_digest = take_run_gate_digest_if_turn_ended(
src/cli/commands.rs:3008:        let gate_digest = take_run_gate_digest_if_turn_ended(
src/cli/commands.rs:3206:        let gate_digest = take_run_gate_digest_if_turn_ended(
src/cli/commands/todo_followup.rs:349:pub(super) fn take_run_gate_digest_if_turn_ended(
crates/jcode-harness-api-server/src/translate.rs:1580:            "turn_ended" => vec![],
crates/jcode-harness-api-server/src/translate.rs:1598:                    Some("turn_event" | "turn_started" | "turn_ended" | "turn_state")
crates/jcode-harness-api-server/src/translate/turn_identity_tests.rs:116:fn turn_ended_is_attribution_only_and_leaves_the_turn_open_for_its_terminal() {
crates/jcode-harness-api-server/src/translate/turn_identity_tests.rs:125:        "type": "turn_ended",
src/cli/acp/daemon_router_tests.rs:94:fn turn_ended(turn_id: u64, status: TurnEndStatus) -> ServerEvent {
src/cli/acp/daemon_router_tests.rs:95:    ServerEvent::TurnEnded {
src/cli/acp/daemon_router_tests.rs:174:            .route(turn_ended(1, TurnEndStatus::Completed))
src/cli/acp/daemon_router_tests.rs:216:    let actions = router.route(turn_ended(9, TurnEndStatus::Completed));
src/cli/acp/daemon_router_tests.rs:219:        [RouteAction::PeerTurnEnded {
src/cli/acp/daemon_router_tests.rs:221:            status: TurnEndStatus::Completed,
src/cli/acp/daemon_router_tests.rs:243:    router.route(turn_ended(1, TurnEndStatus::Completed));
src/cli/acp/daemon_router_tests.rs:257:    let actions = router.route(turn_ended(2, TurnEndStatus::Failed));
src/cli/acp/daemon_router_tests.rs:260:        [RouteAction::PeerTurnEnded {
src/cli/acp/daemon_router_tests.rs:261:            status: TurnEndStatus::Failed,
src/cli/acp/daemon_router_tests.rs:318:    router.route(turn_ended(1, TurnEndStatus::Completed));
src/cli/acp/daemon_router_tests.rs:328:    router.route(turn_ended(2, TurnEndStatus::Completed));
src/cli/acp/daemon_router_tests.rs:355:        r#"{"type":"turn_ended","identity":{"runtime_generation":"gen-a","turn_id":1},"status":"completed"}"#,
src/cli/acp/daemon_router_tests.rs:431:    let actions = router.route(turn_ended(5, TurnEndStatus::Cancelled));
src/cli/acp/daemon_router_tests.rs:434:        [RouteAction::PeerTurnEnded {
src/cli/acp/daemon_router_tests.rs:435:            status: TurnEndStatus::Cancelled,
src/cli/acp/daemon_router_tests.rs:444:    // reports an active turn with no origin, and no turn_ended will ever come.
src/cli/acp/daemon_router_tests.rs:549:    // No turn_ended ever arrives for turn 1. Our own prompt's turn starts.
src/cli/acp/daemon_router_tests.rs:555:            [RouteAction::PeerTurnEnded {
src/cli/acp/daemon_router_tests.rs:557:                status: TurnEndStatus::Cancelled,
src/cli/acp/daemon_router_tests.rs:574:/// A peer's scoped terminal closes a peer turn whose `turn_ended` never
src/cli/acp/daemon_router_tests.rs:586:            [RouteAction::PeerTurnEnded {
src/cli/acp/daemon_router_tests.rs:588:                status: TurnEndStatus::Completed,
src/cli/acp/daemon_router_tests.rs:603:    router.route(turn_ended(1, TurnEndStatus::Completed));
src/cli/acp/daemon_router_tests.rs:624:/// interrupt notice, the turn's `turn_ended(cancelled)` and scoped terminal,
src/cli/acp/daemon_router_tests.rs:641:    canceller.route(turn_ended(1, TurnEndStatus::Cancelled));
src/cli/acp/daemon_router_tests.rs:678:    let actions = peer_router.route(turn_ended(1, TurnEndStatus::Cancelled));
src/cli/acp/daemon_router_tests.rs:682:            [RouteAction::PeerTurnEnded {
src/cli/acp/daemon_router_tests.rs:683:                status: TurnEndStatus::Cancelled,
src/cli/acp/daemon_router_tests.rs:722:            .route(turn_ended(4, TurnEndStatus::Cancelled))
src/cli/acp/daemon_router_tests.rs:724:        [RouteAction::PeerTurnEnded {
src/cli/acp/daemon_router_tests.rs:725:            status: TurnEndStatus::Cancelled,
src/cli/acp/daemon_router_tests.rs:743:    let actions = router.route(turn_ended(1, TurnEndStatus::Cancelled));
src/cli/acp/daemon_router_tests.rs:745:        matches!(actions.as_slice(), [RouteAction::PeerTurnEnded { .. }]),
src/cli/acp/daemon_router_tests.rs:784:            [RouteAction::PeerTurnEnded {
src/cli/acp/daemon_router_tests.rs:785:                status: TurnEndStatus::Cancelled,
src/cli/acp/daemon_router_tests.rs:826:            [RouteAction::PeerTurnEnded {
src/cli/acp/daemon_router_tests.rs:828:                status: TurnEndStatus::Cancelled,
src/cli/acp/daemon_router_tests.rs:1147:    router.route(turn_ended(1, TurnEndStatus::Completed));
src/cli/acp/daemon_router_tests.rs:1194:    router.route(turn_ended(1, TurnEndStatus::Completed));
src/cli/acp/daemon_router_tests.rs:1220:        (TurnEndStatus::Completed, ServerEvent::Done { id: 5 }),
src/cli/acp/daemon_router_tests.rs:1221:        (TurnEndStatus::Cancelled, ServerEvent::Done { id: 5 }),
src/cli/acp/daemon_router_tests.rs:1223:            TurnEndStatus::Failed,
src/cli/acp/daemon_router_tests.rs:1258:        all_actions.extend(router.route(turn_ended(9, status)));
src/cli/acp/daemon_router_tests.rs:1272:                .any(|action| matches!(action, RouteAction::PeerTurnEnded { .. })),
src/cli/acp/daemon_router_tests.rs:1280:            .filter(|event| matches!(event, ServerEvent::TurnEnded { .. }))
src/cli/acp/daemon_router_tests.rs:1285:                [ServerEvent::TurnEnded { identity: ended, status: ended_status, .. }]
src/cli/acp/daemon_router_tests.rs:1292:            .position(|event| matches!(event, ServerEvent::TurnEnded { .. }))
src/cli/acp/daemon_router_tests.rs:1300:            TurnEndStatus::Completed => {
src/cli/acp/daemon_router_tests.rs:1304:            TurnEndStatus::Cancelled => assert!(answered_cancelled(&events, 77), "{events:?}"),
src/cli/acp/daemon_router_tests.rs:1305:            TurnEndStatus::Failed => assert!(
src/cli/acp/daemon_router_tests.rs:1332:    router.route(turn_ended(9, TurnEndStatus::Completed));
src/cli/acp/daemon_router_tests.rs:1359:    let actions = router.route(turn_ended(9, TurnEndStatus::Completed));
src/cli/acp/daemon_router_tests.rs:1363:            .any(|action| matches!(action, RouteAction::PeerTurnEnded { .. }))
src/cli/acp/daemon_router_tests.rs:1388:    router.route(turn_ended(10, TurnEndStatus::Completed));
src/cli/acp/daemon_router_tests.rs:1400:    let actions = router.route(turn_ended(9, TurnEndStatus::Completed));
src/cli/acp/daemon_router_tests.rs:1421:    router.route(turn_ended(10, TurnEndStatus::Completed));
src/cli/acp/daemon_router_tests.rs:1432:        withdrawal(&router.route(turn_ended(11, TurnEndStatus::Completed))).unwrap();
src/cli/acp/daemon_router_tests.rs:1478:    let actions = router.route(turn_ended(9, TurnEndStatus::Cancelled));
src/cli/acp/daemon_router_tests.rs:1482:            .any(|action| matches!(action, RouteAction::PeerTurnEnded { .. }))
src/cli/acp/daemon_router_tests.rs:1545:            ServerEvent::TurnEnded {
src/cli/acp/daemon_router_tests.rs:1546:                status: TurnEndStatus::Cancelled,
src/cli/acp/daemon_router_tests.rs:1557:            .route(turn_ended(9, TurnEndStatus::Completed))
src/cli/acp/daemon_router_tests.rs:1592:    let actions = router.route(turn_ended(9, TurnEndStatus::Completed));
src/cli/acp/daemon_router_tests.rs:1639:            [RouteAction::PeerTurnEnded { turn_id, status: TurnEndStatus::Cancelled, .. }]
src/cli/acp/daemon_router_tests.rs:1653:    r.route(turn_ended(1, TurnEndStatus::Completed));
src/cli/acp/daemon_router_tests.rs:1668:        [RouteAction::PeerTurnEnded {
src/cli/acp/daemon_router_tests.rs:1669:            status: TurnEndStatus::Cancelled,
src/cli/acp/daemon_router_tests.rs:1693:        [RouteAction::PeerTurnEnded {
src/cli/acp/daemon_router_tests.rs:1694:            status: TurnEndStatus::Cancelled,
src/cli/acp/daemon_router.rs:8://! - events scoped to a turn (`turn_started`, `turn_event`, `turn_ended`) are
src/cli/acp/daemon_router.rs:34:use crate::protocol::{TurnEndStatus, TurnIdentity, TurnOrigin};
src/cli/acp/daemon_router.rs:330:    PeerTurnEnded {
src/cli/acp/daemon_router.rs:332:        status: TurnEndStatus,
src/cli/acp/daemon_router.rs:617:        actions.extend(self.close_peer_turn(TurnEndStatus::Cancelled, None));
src/cli/acp/daemon_router.rs:636:                ServerEvent::TurnEnded {
src/cli/acp/daemon_router.rs:638:                    status: TurnEndStatus::Cancelled,
src/cli/acp/daemon_router.rs:859:        actions.extend(self.close_peer_turn(TurnEndStatus::Cancelled, Some(message.to_string())));
src/cli/acp/daemon_router.rs:869:        vec![RouteAction::PeerTurnEnded {
src/cli/acp/daemon_router.rs:871:            status: TurnEndStatus::Cancelled,
src/cli/acp/daemon_router.rs:920:            ServerEvent::TurnEnded {
src/cli/acp/daemon_router.rs:924:            } => self.on_turn_ended(identity, status, message),
src/cli/acp/daemon_router.rs:977:            TurnEndStatus::Cancelled,
src/cli/acp/daemon_router.rs:1010:            TurnEndStatus::Cancelled,
src/cli/acp/daemon_router.rs:1043:    fn on_turn_ended(
src/cli/acp/daemon_router.rs:1046:        status: TurnEndStatus,
src/cli/acp/daemon_router.rs:1061:            if status == TurnEndStatus::Cancelled {
src/cli/acp/daemon_router.rs:1076:            "acp: dropping turn_ended for a turn that is not open: {}",
src/cli/acp/daemon_router.rs:1113:                // Without a `turn_ended` (an older daemon build) the adopted
src/cli/acp/daemon_router.rs:1119:                        ServerEvent::TurnEnded {
src/cli/acp/daemon_router.rs:1140:                // Normally `turn_ended` closed the turn already; this is a
src/cli/acp/daemon_router.rs:1258:        status: TurnEndStatus,
src/cli/acp/daemon_router.rs:1271:            ServerEvent::TurnEnded {
src/cli/acp/daemon_router.rs:1490:            TurnEndStatus::Cancelled,
src/cli/acp/daemon_router.rs:1527:        status: TurnEndStatus,
src/cli/acp/daemon_router.rs:1533:        vec![RouteAction::PeerTurnEnded {
src/cli/acp/daemon_router.rs:1551:/// End status of a turn whose scoped terminal arrived without `turn_ended`.
src/cli/acp/daemon_router.rs:1552:fn terminal_status(event: &ServerEvent) -> (TurnEndStatus, Option<String>) {
src/cli/acp/daemon_router.rs:1554:        ServerEvent::Error { message, .. } => (TurnEndStatus::Failed, Some(message.clone())),
src/cli/acp/daemon_router.rs:1555:        _ => (TurnEndStatus::Completed, None),
src/cli/acp/daemon_router.rs:1584:            | ServerEvent::TurnEnded { .. }
src/cli/acp/tests.rs:1197:        RouteAction::PeerTurnEnded { turn_id, .. } => format!("ended {turn_id}"),
src/cli/acp/tests.rs:1282:        assert!(gate.hold(RouteAction::PeerTurnEnded {
src/cli/acp/tests.rs:1284:            status: TurnEndStatus::Completed,
src/cli/acp/tests.rs:1447:    router.route(ServerEvent::TurnEnded {
src/cli/acp/tests.rs:1449:        status: TurnEndStatus::Completed,
src/cli/acp/tests.rs:1897:    assert_eq!(turn_end_state(TurnEndStatus::Cancelled), "canceled");
crates/jcode-protocol/src/wire.rs:917:    TurnEndStatus, TurnIdentity, TurnOrigin, TurnOriginKind, server_capabilities,
crates/jcode-protocol/src/lib.rs:363:    TurnEndStatus, TurnIdentity, TurnOrigin, TurnOriginKind, server_capabilities,
crates/jcode-protocol/src/wire/events.rs:83:/// origin and prompt and every turn is closed by `turn_ended`.
crates/jcode-protocol/src/wire/events.rs:157:pub enum TurnEndStatus {
crates/jcode-protocol/src/wire/events.rs:198:    #[serde(rename = "turn_ended")]
crates/jcode-protocol/src/wire/events.rs:199:    TurnEnded {
crates/jcode-protocol/src/wire/events.rs:201:        status: TurnEndStatus,
crates/jcode-protocol/src/protocol_tests/turn_identity.rs:143:        (crate::TurnEndStatus::Completed, "completed"),
crates/jcode-protocol/src/protocol_tests/turn_identity.rs:144:        (crate::TurnEndStatus::Failed, "failed"),
crates/jcode-protocol/src/protocol_tests/turn_identity.rs:145:        (crate::TurnEndStatus::Cancelled, "cancelled"),
crates/jcode-protocol/src/protocol_tests/turn_identity.rs:147:        let message = (status == crate::TurnEndStatus::Failed).then(|| "boom".to_string());
crates/jcode-protocol/src/protocol_tests/turn_identity.rs:148:        let ended = ServerEvent::TurnEnded {
crates/jcode-protocol/src/protocol_tests/turn_identity.rs:154:        assert_eq!(value["type"], "turn_ended");
crates/jcode-protocol/src/protocol_tests/turn_identity.rs:160:            ServerEvent::TurnEnded { identity: id, status: decoded_status, message: decoded_message }
crates/jcode-tui/src/tui/app/remote/server_events.rs:234:                    | ServerEvent::TurnEnded { .. }
crates/jcode-tui/src/tui/app/remote/server_events.rs:1121:        // `turn_ended` is always followed by the turn's scoped terminal, which
crates/jcode-tui/src/tui/app/remote/server_events.rs:1125:        | ServerEvent::TurnEnded { .. }
crates/jcode-app-core/src/server/state.rs:539:    /// preceded by `turn_ended`.
crates/jcode-app-core/src/server/state.rs:550:                    crate::protocol::TurnEndStatus::Failed,
crates/jcode-app-core/src/server/state.rs:553:                _ if self.cancel.cancelled() => (crate::protocol::TurnEndStatus::Cancelled, None),
crates/jcode-app-core/src/server/state.rs:554:                _ => (crate::protocol::TurnEndStatus::Completed, None),
crates/jcode-app-core/src/server/state.rs:565:    async fn end(&mut self, status: crate::protocol::TurnEndStatus, message: Option<String>) {
crates/jcode-app-core/src/server/state.rs:567:        // Retire first: a client attaching after `turn_ended` must not be told
crates/jcode-app-core/src/server/state.rs:570:        self.publish(ServerEvent::TurnEnded {
crates/jcode-app-core/src/server/state.rs:591:                    crate::protocol::TurnEndStatus::Cancelled,
crates/jcode-app-core/src/server/state.rs:612:/// `turn_ended` immediately followed by the scoped terminal (`done`/`error`).
crates/jcode-app-core/src/server/state.rs:618:/// `turn_started` is queued, and withdrawn before `turn_ended`.
crates/jcode-app-core/src/server/comm_control.rs:616:                    "task_turn_ended"
crates/jcode-app-core/src/server/client_actions_tests.rs:1173:/// its own `turn_started`/`turn_ended` with a client origin, and every event in
crates/jcode-app-core/src/server/client_actions_tests.rs:1200:        .position(|event| matches!(event, ServerEvent::TurnEnded { .. }))
crates/jcode-app-core/src/server/client_actions_tests.rs:1214:        "turn_started → tool stream → turn_ended → terminal: {events:?}"
crates/jcode-app-core/src/server/client_actions_tests.rs:1239:            | ServerEvent::TurnEnded {
crates/jcode-app-core/src/server/client_lifecycle_tests.rs:38:    // Legacy readers get the terminal that follows `turn_ended`, never the
crates/jcode-app-core/src/server/client_lifecycle_tests.rs:40:    let ended = ServerEvent::TurnEnded {
crates/jcode-app-core/src/server/client_lifecycle_tests.rs:42:        status: crate::protocol::TurnEndStatus::Completed,
crates/jcode-app-core/src/server/client_lifecycle_tests.rs:48:        Some(ServerEvent::TurnEnded { .. })
crates/jcode-app-core/src/server/client_lifecycle_tests.rs:1028:            Some(ServerEvent::TurnEnded {
crates/jcode-app-core/src/server/client_lifecycle_tests.rs:1029:                status: crate::protocol::TurnEndStatus::Completed,
crates/jcode-app-core/src/server/client_lifecycle.rs:95:            // Identity-only bookkeeping. `turn_ended` is always followed by the
crates/jcode-app-core/src/server/client_lifecycle.rs:98:            | ServerEvent::TurnEnded { .. }
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:321:                ServerEvent::TurnEnded {
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:323:                    status: crate::protocol::TurnEndStatus::Cancelled,
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:403:            ServerEvent::TurnEnded {
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:405:                status: crate::protocol::TurnEndStatus::Cancelled,
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:495:            read_until(rx, |event| matches!(event, ServerEvent::TurnEnded { .. })).await;
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:515:            .position(|event| matches!(event, ServerEvent::TurnEnded { .. }))
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:520:                ServerEvent::TurnEnded {
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:522:                    status: crate::protocol::TurnEndStatus::Cancelled,
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:532:            "the channel's terminal follows turn_ended: {events:?}"
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:746:            ServerEvent::TurnEnded { status, .. } => Some(format!("ended {status:?}")),
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:768:/// before `turn_started` and withdrawn before `turn_ended`.
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:828:            if matches!(event, ServerEvent::TurnEnded { .. }) {
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:831:                    "the turn is withdrawn from the attach snapshot before turn_ended"
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:852:            | ServerEvent::TurnEnded { identity: id, .. }
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:964:                ServerEvent::TurnEnded { identity, .. } => {
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:982:/// A panicking turn still ends: every attachment sees a failed `turn_ended`
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:1023:                ServerEvent::TurnEnded {
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:1024:                    status: crate::protocol::TurnEndStatus::Failed,
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:1034:                .filter(|event| matches!(event, ServerEvent::TurnEnded { .. }))
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:1093:        ServerEvent::TurnEnded {
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:1094:            status: crate::protocol::TurnEndStatus::Completed,
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:1101:        "withdrawn before turn_ended"
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:1214:            ServerEvent::TurnEnded {
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:1216:                status: crate::protocol::TurnEndStatus::Completed,
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:1292:            ServerEvent::TurnEnded {
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:1294:                status: crate::protocol::TurnEndStatus::Failed,
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:1400:        ServerEvent::TurnEnded {
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:1401:            status: crate::protocol::TurnEndStatus::Completed,
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:1492:    assert!(normal.iter().any(|event| matches!(event, ServerEvent::TurnEnded {
crates/jcode-app-core/src/server/turn_event_channel_tests.rs:1493:        identity: ended, status: crate::protocol::TurnEndStatus::Completed, .. } if ended == &identity)));
```

## Drain bookkeeping and ownership inventory

```text
crates/jcode-app-core/src/server/state.rs:525:    cancel: crate::agent::CancelProbe,
crates/jcode-app-core/src/server/state.rs:527:    ended: bool,
crates/jcode-app-core/src/server/state.rs:539:    /// preceded by `turn_ended`.
crates/jcode-app-core/src/server/state.rs:547:        if !self.ended && is_turn_terminal(&event) {
crates/jcode-app-core/src/server/state.rs:566:        self.ended = true;
crates/jcode-app-core/src/server/state.rs:567:        // Retire first: a client attaching after `turn_ended` must not be told
crates/jcode-app-core/src/server/state.rs:569:        self.publication.retire();
crates/jcode-app-core/src/server/state.rs:580:        if self.ended {
crates/jcode-app-core/src/server/state.rs:592:                    Some("The turn ended before it reported completion.".into()),
crates/jcode-app-core/src/server/state.rs:612:/// `turn_ended` immediately followed by the scoped terminal (`done`/`error`).
crates/jcode-app-core/src/server/state.rs:618:/// `turn_started` is queued, and withdrawn before `turn_ended`.
crates/jcode-app-core/src/server/state.rs:649:            cancel: lease.cancel_probe(),
crates/jcode-app-core/src/server/state.rs:651:            ended: false,
crates/jcode-app-core/src/agent/turn_ownership.rs:194:    pub(crate) fn cancel_probe(&self) -> CancelProbe {
crates/jcode-app-core/src/agent/turn_ownership.rs:195:        CancelProbe(self.cancel.clone())
crates/jcode-app-core/src/agent/turn_ownership.rs:207:/// See [`TurnLease::cancel_probe`].
crates/jcode-app-core/src/agent/turn_ownership.rs:209:pub(crate) struct CancelProbe(Option<(InterruptSignal, u64)>);
crates/jcode-app-core/src/agent/turn_ownership.rs:211:impl CancelProbe {
crates/jcode-app-core/src/agent/turn_ownership.rs:222:    /// so a client attaching afterwards does not open a turn that has ended.
crates/jcode-app-core/src/agent/turn_ownership.rs:224:    pub(crate) fn retire(&self) {
crates/jcode-app-core/src/agent.rs:27:    CancelProbe, RuntimeMutationLease, TurnAttribution, TurnLease, TurnObserver, TurnPublicationPin,
```

## Before core diff

```text
 crates/jcode-app-core/src/agent.rs                 |    9 +-
 crates/jcode-app-core/src/agent/interrupts.rs      |    9 +
 crates/jcode-app-core/src/agent/turn_execution.rs  |    3 +
 crates/jcode-app-core/src/agent/turn_ownership.rs  |  120 +-
 .../src/agent/turn_streaming_mpsc.rs               |    7 +
 .../src/protocol_tests/core_events.rs              |    1 +
 crates/jcode-app-core/src/server.rs                |    5 +
 .../jcode-app-core/src/server/background_tasks.rs  |    4 +
 crates/jcode-app-core/src/server/client_actions.rs |  314 ++--
 .../src/server/client_actions_tests.rs             |  207 ++-
 .../src/server/client_comm_message.rs              |    1 +
 .../jcode-app-core/src/server/client_lifecycle.rs  |  113 +-
 .../src/server/client_lifecycle_logging.rs         |    2 +-
 .../src/server/client_lifecycle_socket_tests.rs    |    2 +-
 .../src/server/client_lifecycle_tests.rs           |  301 +++-
 .../jcode-app-core/src/server/client_processing.rs |  138 +-
 crates/jcode-app-core/src/server/client_session.rs |   16 +
 crates/jcode-app-core/src/server/client_state.rs   |   83 +-
 .../src/server/client_state_tests.rs               |   42 +
 .../src/server/client_state_tests/history_delta.rs |    1 +
 crates/jcode-app-core/src/server/comm_control.rs   |    4 +
 .../comm_control_tests/progress_publication.rs     |    3 +
 crates/jcode-app-core/src/server/comm_session.rs   |    3 +
 .../src/server/history_provider_tests.rs           |   27 +
 crates/jcode-app-core/src/server/jade_relay.rs     |   10 +-
 crates/jcode-app-core/src/server/live_turn.rs      |   13 +-
 .../jcode-app-core/src/server/provider_control.rs  |   10 +
 .../src/server/session_connections.rs              |    7 +-
 crates/jcode-app-core/src/server/state.rs          |  199 ++-
 crates/jcode-app-core/src/server/tests.rs          |    2 +
 .../src/server/turn_event_channel_tests.rs         | 1526 ++++++++++++++++++++
 crates/jcode-app-core/src/server/turn_failover.rs  |    8 +
 .../jcode-app-core/src/tool/communicate_tests.rs   |    3 +
 crates/jcode-app-core/src/tool/sdk.rs              |   37 +-
 crates/jcode-app-core/src/update.rs                |    5 +-
 crates/jcode-base/src/provider/mod.rs              |   71 +-
 crates/jcode-base/src/provider/tests.rs            |   17 +
 .../src/provider/tests/model_resolution.rs         |   77 +-
 .../jcode-base/src/provider/tests/startup_pool.rs  |   20 +
 crates/jcode-harness-api-server/src/lib.rs         |    5 +
 crates/jcode-harness-api-server/src/translate.rs   |    6 +-
 .../src/translate/session_catalog.rs               |  101 ++
 .../src/translate/session_catalog_tests.rs         |  218 +++
 .../src/translate/turn_identity_tests.rs           |   23 +
 .../src/translate_tests.rs                         |    1 +
 crates/jcode-protocol/src/comm_format.rs           |    1 +
 crates/jcode-protocol/src/lib.rs                   |   16 +-
 .../src/protocol_tests/core_events.rs              |   68 +
 .../src/protocol_tests/turn_identity.rs            |  197 +++
 crates/jcode-protocol/src/wire.rs                  |   22 +-
 crates/jcode-protocol/src/wire/events.rs           |  114 +-
 crates/jcode-provider-core/src/lib.rs              |    2 +-
 crates/jcode-provider-core/src/reasoning.rs        |   50 +-
 crates/jcode-tui/src/tui/app.rs                    |    4 +
 .../jcode-tui/src/tui/app/auth_account_picker.rs   |    2 +
 .../src/tui/app/auth_remote/onboarding.rs          |    1 +
 crates/jcode-tui/src/tui/app/auth_remote/picker.rs |    1 +
 crates/jcode-tui/src/tui/app/inline_interactive.rs |   18 +-
 .../src/tui/app/inline_interactive/openers.rs      |    2 +
 .../src/tui/app/inline_interactive/preview.rs      |   39 +
 .../src/tui/app/inline_interactive_tests.rs        |    6 +
 crates/jcode-tui/src/tui/app/input_help.rs         |    2 +-
 crates/jcode-tui/src/tui/app/remote.rs             |   47 +-
 .../jcode-tui/src/tui/app/remote/input_dispatch.rs |    5 +-
 .../jcode-tui/src/tui/app/remote/server_events.rs  |   27 +-
 .../remote/server_events/history_dedup_tests.rs    |    1 +
 .../src/tui/app/remote/turn_identity_tests.rs      |   16 +
 crates/jcode-tui/src/tui/app/remote_tests.rs       |  122 ++
 .../src/tui/app/remote_tests/swarm_delivery.rs     |    1 +
 crates/jcode-tui/src/tui/app/tests.rs              |    9 +
 .../tui/app/tests/idle_soft_interrupt_cancel.rs    |    2 +
 .../tui/app/tests/model_picker_startup_tests.rs    |  109 +-
 .../src/tui/app/tests/reload_recovery_replay.rs    |    2 +
 .../app/tests/remote_events_reload_01/part_01.rs   |   14 +
 .../app/tests/remote_events_reload_01/part_02.rs   |    2 +
 .../app/tests/remote_events_reload_03/part_01.rs   |   10 +
 .../app/tests/remote_events_reload_03/part_02.rs   |    9 +
 .../src/tui/app/tests/remote_events_reload_04.rs   |    3 +
 .../src/tui/app/tests/remote_failover_routes.rs    |    4 +
 .../src/tui/app/tests/remote_header_hint_facts.rs  |    2 +
 .../tui/app/tests/remote_model_picker_hotkeys.rs   |   25 +-
 .../app/tests/remote_startup_input_01/part_01.rs   |    2 +
 .../app/tests/remote_startup_input_01/part_02.rs   |    1 +
 .../app/tests/remote_startup_input_03/part_02.rs   |    3 +
 crates/jcode-tui/src/tui/app/tests/ssh_remote.rs   |    1 +
 .../src/tui/app/tests/swarm_plan_graph_inline.rs   |    1 +
 crates/jcode-tui/src/tui/app/tui_lifecycle.rs      |   24 +
 crates/jcode-tui/src/tui/app/usage_reset.rs        |    1 +
 crates/jcode-tui/src/tui/backend.rs                |    3 +-
 crates/jcode-tui/src/tui/mod.rs                    |    2 +
 .../src/tui/ui_inline_interactive_tests.rs         |    3 +
 .../src/tui/ui_tests/basic/frame_flicker.rs        |    1 +
 crates/jcode-tui/src/tui/ui_tests/inline_picker.rs |    1 +
 93 files changed, 4476 insertions(+), 296 deletions(-)
```

## Verification

No fixture was re-recorded. No lifecycle behavior has been changed yet.
