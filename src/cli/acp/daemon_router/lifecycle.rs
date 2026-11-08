// Turn lifecycle routing. Included in daemon_router to preserve its private scope.
impl RouterState {
    /// Route one daemon event. Prompt and reply delivery happen through the
    /// registered channels; the returned actions are what the adapter must
    /// render for other clients' activity.
    pub(super) fn route(&mut self, event: ServerEvent) -> Vec<RouteAction> {
        if !self.turn_origin {
            return self.route_legacy(event);
        }
        match event {
            ServerEvent::TurnState {
                session_id,
                runtime_generation,
                turn_id,
                latest_turn_id,
                active_origin,
                active_user_content,
            } => self.on_turn_state(
                session_id,
                runtime_generation,
                turn_id,
                latest_turn_id,
                active_origin,
                active_user_content,
            ),
            ServerEvent::TurnStarted {
                identity,
                origin,
                user_content,
            } => self.on_turn_started(identity, origin, user_content),
            ServerEvent::TurnEnded {
                identity,
                status,
                message,
            } => self.on_turn_ended(identity, status, message),
            ServerEvent::TurnEvent { identity, event } => self.on_scoped(identity, *event),
            event => self.on_flat(event),
        }
    }

    /// `turn_state` precedes every `History`: it says which turn, if any, is
    /// running right now. A running peer turn this connection has not seen
    /// start (a mid-turn attach) is opened here.
    fn on_turn_state(
        &mut self,
        session_id: String,
        runtime_generation: String,
        turn_id: Option<u64>,
        latest_turn_id: u64,
        active_origin: Option<TurnOrigin>,
        active_user_content: Option<String>,
    ) -> Vec<RouteAction> {
        let Some(replaced) = self
            .fence
            .bootstrap(&runtime_generation, latest_turn_id, turn_id)
        else {
            crate::logging::debug("acp: ignoring a stale turn_state");
            return Vec::new();
        };
        self.session_id = session_id;
        let mut actions = Vec::new();
        if replaced {
            actions.extend(self.retire_runtime());
        }
        let (Some(turn_id), Some(origin)) = (turn_id, active_origin) else {
            // No active turn, or one nobody announces (a debug path that
            // claims a turn id without publishing): nothing to mirror.
            return actions;
        };
        let identity = TurnIdentity {
            runtime_generation,
            turn_id,
        };
        if !self.fence.admits(&identity) || self.is_open(&identity) {
            return actions;
        }
        if self.is_own_origin(&origin) {
            // A snapshot taken during our own prompt (a catalog refresh) whose
            // `turn_started` this connection somehow missed.
            let request_id = origin.request_id.unwrap_or_default();
            self.waiting.remove(&request_id);
            self.correlations
                .insert(request_id, (turn_id_string(&identity), "origin"));
            self.own_turn = Some(OwnTurn::started(identity, request_id));
            return actions;
        }
        actions.extend(self.close_peer_turn(
            TurnEndStatus::Cancelled,
            Some("The turn ended without a lifecycle end event.".to_string()),
        ));
        actions.extend(self.open_peer_turn(identity, origin, active_user_content));
        actions
    }

    fn on_turn_started(
        &mut self,
        identity: TurnIdentity,
        origin: Option<TurnOrigin>,
        user_content: Option<String>,
    ) -> Vec<RouteAction> {
        if self.is_open(&identity) {
            // Already opened from an attach snapshot.
            return Vec::new();
        }
        let Some(replaced) = self.fence.start(&identity) else {
            crate::logging::debug(&format!(
                "acp: dropping turn_started for fenced turn {}",
                turn_id_string(&identity)
            ));
            return Vec::new();
        };
        let mut actions = Vec::new();
        if replaced {
            actions.extend(self.retire_runtime());
        }
        // Turns never overlap within a runtime, so a peer turn still open here
        // never reported its end (an aborted or panicking peer). Close it
        // before opening anything else, or the new turn's stream would be
        // mirrored as that stale peer turn's output.
        actions.extend(self.close_peer_turn(
            TurnEndStatus::Cancelled,
            Some("The turn ended without a lifecycle end event.".to_string()),
        ));
        // A daemon advertising `turn_origin` always sends one; without it the
        // turn cannot be ours.
        let origin = origin
            .unwrap_or_else(|| TurnOrigin::server(crate::protocol::TurnOriginKind::Background));
        if self.is_own_origin(&origin) {
            // Our prompt started this turn (a `Message`, or a steer the daemon
            // ran at once because the session was idle).
            let request_id = origin.request_id.unwrap_or_default();
            self.waiting.remove(&request_id);
            if self.view_closed && !self.detached {
                // Sent before `session/close` and started after it: the close
                // rule cancels this view's own turn.
                actions.push(RouteAction::Send(Request::Cancel {
                    id: self.next_request_id(),
                    turn: Some(identity.clone()),
                }));
            }
            self.correlations
                .insert(request_id, (turn_id_string(&identity), "origin"));
            if self.detached {
                self.resolve_cancelled(request_id);
            } else {
                self.own_turn = Some(OwnTurn::started(identity, request_id));
            }
        } else {
            actions.extend(self.open_peer_turn(identity, origin, user_content));
        }
        actions
    }

    fn on_turn_ended(
        &mut self,
        identity: TurnIdentity,
        status: TurnEndStatus,
        message: Option<String>,
    ) -> Vec<RouteAction> {
        if let Some(own) = self
            .own_turn
            .as_ref()
            .filter(|own| own.identity == identity)
        {
            // Keep `own_turn` until the scoped terminal: it is the prompt's
            // completion. A cancelled turn tells the prompt so, because the
            // cancel path's own `interrupted` reaches this connection only
            // after the turn's terminal (and only if this connection
            // cancelled).
            let members: Vec<u64> = own.members().collect();
            let primary = own.request_id;
            if status == TurnEndStatus::Cancelled {
                for member in members {
                    self.deliver(member, ServerEvent::Interrupted);
                }
            }
            self.end_adopted_bracket(primary, identity, status, message);
            return Vec::new();
        }
        if self.peer_matches(&identity) {
            self.fence.complete(&identity);
            let mut actions = self.close_peer_turn(status, message);
            actions.extend(self.on_turn_closed());
            return actions;
        }
        crate::logging::debug(&format!(
            "acp: dropping turn_ended for a turn that is not open: {}",
            turn_id_string(&identity)
        ));
        Vec::new()
    }

    /// An event inside a turn's scope. It belongs to that turn and nothing
    /// else: the own prompt, the mirrored peer turn, or (for a turn this
    /// connection does not have open, such as a peer's second terminal after a
    /// forced abort) nobody.
    fn on_scoped(&mut self, identity: TurnIdentity, event: ServerEvent) -> Vec<RouteAction> {
        let event = match flatten_scope(event) {
            Some(event) => event,
            None => {
                crate::logging::debug("acp: dropping a nested turn scope event");
                return Vec::new();
            }
        };
        if let ServerEvent::SoftInterruptInjected {
            content,
            client_ids,
            ..
        } = &event
        {
            let (content, client_ids) = (content.clone(), client_ids.clone());
            return self.on_injected(identity, event, content, &client_ids);
        }
        let terminal = matches!(event, ServerEvent::Done { .. } | ServerEvent::Error { .. });
        if let Some(own) = self
            .own_turn
            .as_ref()
            .filter(|own| own.identity == identity)
        {
            let request_id = own.request_id;
            if terminal {
                let own = self.own_turn.take().expect("matched above");
                self.fence.complete(&identity);
                // Without a `turn_ended` (an older daemon build) the adopted
                // turn's bracket is still open.
                if own.bracket_open {
                    let (status, message) = terminal_status(&event);
                    self.deliver(
                        own.request_id,
                        ServerEvent::TurnEnded {
                            identity,
                            status,
                            message,
                        },
                    );
                }
                // Every prompt the turn answers resolves with it; a terminal
                // carries the id of the request that started the turn.
                for member in own.members() {
                    self.deliver(member, retarget_terminal(&event, member));
                }
                return self.on_turn_closed();
            } else if let Some(change) = session_state_change(&event) {
                return vec![RouteAction::SessionState(change)];
            }
            self.deliver(request_id, event);
            return Vec::new();
        }
        if self.peer_matches(&identity) {
            if terminal {
                // Normally `turn_ended` closed the turn already; this is a
                // terminal without one (an older daemon build).
                self.fence.complete(&identity);
                let (status, message) = terminal_status(&event);
                let mut actions = self.close_peer_turn(status, message);
                actions.extend(self.on_turn_closed());
                return actions;
            }
            if let Some(change) = session_state_change(&event) {
                return vec![RouteAction::SessionState(change)];
            }
            return vec![RouteAction::PeerTurnEvent(event)];
        }
        crate::logging::debug(&format!(
            "acp: dropping an event scoped to a turn that is not open: {}",
            turn_id_string(&identity)
        ));
        Vec::new()
    }

    /// A steer was injected into turn `identity`.
    ///
    /// Ours (its client ids name a waiting prompt) joins our own turn, or
    /// makes a mirrored peer turn ours. Anyone else's is shown as a user
    /// message: a peer turn's by the mirror, our own turn's by its prompt. A
    /// group that combines our steer with another client's is treated as ours,
    /// so our text is never echoed back as a peer message.
    fn on_injected(
        &mut self,
        identity: TurnIdentity,
        event: ServerEvent,
        content: String,
        client_ids: &[String],
    ) -> Vec<RouteAction> {
        let ours = self.take_waiting(client_ids);
        for prompt_id in &ours {
            self.correlations
                .insert(*prompt_id, (turn_id_string(&identity), "steer"));
        }
        if let Some(own) = self
            .own_turn
            .as_mut()
            .filter(|own| own.identity == identity)
        {
            if ours.is_empty() {
                let request_id = own.request_id;
                self.deliver(request_id, event);
            } else {
                own.joined.extend(ours);
            }
            return Vec::new();
        }
        if self.peer_matches(&identity) {
            if ours.is_empty() {
                let peer = self.peer_turn.as_mut().expect("matched above");
                let index = peer.steers;
                peer.steers += 1;
                return vec![RouteAction::PeerSteer {
                    turn_id: turn_id_string(&identity),
                    index,
                    content,
                }];
            }
            if self.view_closed {
                // The view closed while the steer waited: the turn stays a
                // peer turn.
                for prompt_id in ours {
                    self.resolve_cancelled(prompt_id);
                }
                return Vec::new();
            }
            // Adopt the turn: from here on its output answers our prompt.
            let peer = self.peer_turn.take().expect("matched above");
            let turn_id = turn_id_string(&peer.identity);
            let mut members = ours.into_iter();
            let request_id = members.next().expect("not empty");
            self.own_turn = Some(OwnTurn {
                identity: peer.identity,
                request_id,
                joined: members.collect(),
                adopted: true,
                bracket_open: true,
            });
            return vec![RouteAction::PeerTurnAdopted { turn_id }];
        }
        if ours.is_empty() {
            crate::logging::debug(&format!(
                "acp: dropping a steer injected into a turn that is not open: {}",
                turn_id_string(&identity)
            ));
            return Vec::new();
        }
        if self.own_turn.is_some() || self.view_closed || !self.fence.admits(&identity) {
            for prompt_id in ours {
                self.resolve_cancelled(prompt_id);
            }
            return Vec::new();
        }
        // A turn this view never saw start took our steer; it still answers
        // the prompt, without a bracket.
        let mut members = ours.into_iter();
        let request_id = members.next().expect("not empty");
        self.own_turn = Some(OwnTurn {
            identity,
            request_id,
            joined: members.collect(),
            adopted: true,
            bracket_open: false,
        });
        Vec::new()
    }

    /// Hand an adopted turn's owed `_jcode/turn` terminal to its prompt, which
    /// writes it after the turn's remaining output.
    fn end_adopted_bracket(
        &mut self,
        request_id: u64,
        identity: TurnIdentity,
        status: TurnEndStatus,
        message: Option<String>,
    ) {
        let Some(own) = self
            .own_turn
            .as_mut()
            .filter(|own| own.identity == identity && own.bracket_open)
        else {
            return;
        };
        own.bracket_open = false;
        self.deliver(
            request_id,
            ServerEvent::TurnEnded {
                identity,
                status,
                message,
            },
        );
    }
}
