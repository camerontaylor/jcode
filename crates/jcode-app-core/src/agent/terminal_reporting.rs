use super::Agent;

pub(crate) struct TerminalReport(Box<dyn FnOnce() + Send>);

impl TerminalReport {
    pub(crate) fn emit(self) {
        (self.0)();
    }
}

impl Agent {
    /// Mark this agent session as closed and persist it.
    pub fn mark_closed(&mut self) {
        self.mark_closed_with_reason(crate::telemetry::SessionEndReason::NormalExit);
    }

    /// Close the current session because this Agent is about to adopt another
    /// one in-process (resume after reconnect). Nothing is exiting, so lifecycle
    /// telemetry goes to the ordered background worker instead of two bounded
    /// blocking flushes (up to ~800ms each), which stalled every reload
    /// reconnect's restore by 0.4-2s.
    pub(crate) fn mark_closed_for_replacement(&mut self) {
        if let Some(mut guard) = self.concurrency_session.take() {
            guard.finish_background();
        }
        self.mark_closed_with_reason(crate::telemetry::SessionEndReason::Superseded);
        // The adopted session continues under whatever telemetry state this
        // Agent had. An uncommitted placeholder's never-begun session is moot.
        self.telemetry_session_deferred = false;
    }

    pub(super) fn begin_telemetry_session(&self) {
        crate::telemetry::begin_session_with_parent(
            self.provider.name(),
            &self.provider.model(),
            self.session.parent_id.clone(),
            false,
        );
    }

    fn mark_closed_with_reason(&mut self, end_reason: crate::telemetry::SessionEndReason) {
        if let Some(report) = self.prepare_closed_reporting(end_reason, false) {
            report.emit();
        }
    }

    pub(crate) fn prepare_closed_reporting(
        &mut self,
        end_reason: crate::telemetry::SessionEndReason,
        background: bool,
    ) -> Option<TerminalReport> {
        self.finish_concurrency_tracking();
        self.persist_soft_interrupt_snapshot();
        self.session.mark_closed();
        if !self.session.messages.is_empty() {
            self.persist_session_best_effort("session close state");
        }
        Some(self.prepare_terminal_report(end_reason, false, background))
    }

    /// Fire a session lifecycle observer hook (`session_start`/`session_end`).
    /// No-op when the hook is not configured.
    pub(crate) fn fire_session_lifecycle_hook(&self, event_name: &'static str, source: &str) {
        if !crate::hooks::hook_configured(event_name) {
            return;
        }
        let mut event = crate::hooks::HookEvent::new(event_name)
            .session_id(self.session.id.clone())
            .field("SOURCE", source)
            .field("MODEL", self.provider_model());
        if let Some(cwd) = self.working_dir() {
            event = event.cwd(cwd);
        }
        crate::hooks::dispatch_observer(event);
    }

    pub fn mark_crashed(&mut self, message: Option<String>) {
        if let Some(report) = self.prepare_crashed_reporting(message, false) {
            report.emit();
        }
    }

    pub(crate) fn prepare_crashed_reporting(
        &mut self,
        message: Option<String>,
        background: bool,
    ) -> Option<TerminalReport> {
        self.finish_concurrency_tracking();
        self.persist_soft_interrupt_snapshot();
        self.session.mark_crashed(message);
        if !self.session.messages.is_empty() {
            self.persist_session_best_effort("session crash state");
        }
        Some(self.prepare_terminal_report(
            crate::telemetry::SessionEndReason::Unknown,
            true,
            background,
        ))
    }

    fn prepare_terminal_report(
        &mut self,
        reason: crate::telemetry::SessionEndReason,
        crashed: bool,
        background: bool,
    ) -> TerminalReport {
        let transcript = if background {
            if self.transcript_telemetry_sent || self.session.messages.is_empty() {
                None
            } else {
                // This retiring Agent schedules its transcript at most once.
                // Consent, installation-ID I/O and submission run on the worker.
                self.transcript_telemetry_sent = true;
                Some(self.session.redacted_for_export())
            }
        } else {
            self.upload_transcript_telemetry(reason);
            None
        };
        let provider = self.provider.name().to_owned();
        let model = self.provider.model();
        let lifecycle = if crashed || !self.telemetry_session_deferred {
            crate::telemetry::prepare_session_end(&provider, &model, reason, crashed, background)
        } else {
            None
        };
        let observer = (!crashed).then(|| {
            let event = crate::hooks::HookEvent::new("session_end")
                .session_id(self.session.id.clone())
                .field("SOURCE", "close")
                .field("MODEL", self.provider_model());
            if let Some(cwd) = self.working_dir() {
                event.cwd(cwd)
            } else {
                event
            }
        });
        TerminalReport(Box::new(move || {
            if let Some(session) = transcript {
                match serde_json::to_value(&session.messages) {
                    Ok(messages) => {
                        crate::telemetry::record_transcript(&provider, &model, reason, messages);
                    }
                    Err(error) => crate::logging::warn(&format!(
                        "failed to serialize consented transcript telemetry: {error}"
                    )),
                }
            }
            if let Some(report) = lifecycle {
                report.emit();
            }
            if let Some(event) = observer
                && crate::hooks::hook_configured("session_end")
            {
                crate::hooks::dispatch_observer(event);
            }
        }))
    }
}
