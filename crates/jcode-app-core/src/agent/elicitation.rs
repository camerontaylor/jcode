//! Session UI capability changes invalidate the provider's locked tool surface.
use super::*;
impl Agent {
    pub(crate) async fn set_form_capabilities(&mut self, desktop: bool, applet: bool) {
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
}
