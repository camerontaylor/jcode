//! Session UI capability changes register form tools and invalidate the locked
//! provider tool surface.
use super::*;
impl Agent {
    pub(crate) async fn set_form_capabilities(&mut self, desktop: bool, applet: bool) {
        self.register_ask_user().await;
        if self.ui_capabilities == (desktop, applet) {
            return;
        }
        self.ui_capabilities = (desktop, applet);
        self.registry.set_desktop_ui_enabled(desktop).await;
        if !desktop && applet {
            self.registry
                .register(
                    "applet".into(),
                    Arc::new(crate::tool::applet_elicit::AppletElicitationTool),
                )
                .await;
        }
        let mut allowed = self.allowed_tools.clone();
        if let Some(allowed) = allowed.as_mut() {
            if applet {
                allowed.insert("applet".into());
            }
            if desktop {
                allowed.insert("user_response".into());
            }
        }
        self._tool_policy_registration = crate::tool::register_session_tool_policy(
            &self.session.id,
            allowed,
            self.disabled_tools.clone(),
        );
        self.invalidate_sdk_tools();
    }

    /// Register `ask_user` once any form-capable client attaches, before the
    /// `(desktop, applet)` early return: a TUI reports `(false, false)`.
    ///
    /// The registry is the source of truth, so `Agent` needs no extra field and
    /// detaching never unregisters the tool. Registration invalidates the locked
    /// surface once, because the first request may already have locked it.
    async fn register_ask_user(&mut self) {
        if !crate::elicitation::global().forms(&self.session.id) {
            return;
        }
        if self
            .registry
            .tool_names()
            .await
            .iter()
            .any(|name| name == "ask_user")
        {
            return;
        }
        self.registry
            .register(
                "ask_user".into(),
                Arc::new(crate::elicitation::ask_user::AskUserTool),
            )
            .await;
        self.invalidate_sdk_tools();
    }
}
