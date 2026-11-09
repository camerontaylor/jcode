use super::*;
#[tokio::test]
async fn applet_elicitation_capability_updates_locked_tool_surface() {
    let mut agent = Agent::new(Arc::new(BoundSavedAccountProvider), Registry::empty());
    agent.allowed_tools = Some(["bash".to_string()].into_iter().collect());
    assert!(
        !agent
            .tool_definitions()
            .await
            .iter()
            .any(|d| d.name == "applet")
    );
    agent.set_form_capabilities(false, true).await;
    assert!(agent.locked_tools.is_none());
    assert_eq!(
        crate::tool::session_tool_policy_allows_tool_for_test(agent.session_id(), "applet"),
        Some(true)
    );
    let definitions = agent.tool_definitions().await;
    let applet = definitions.iter().find(|d| d.name == "applet").unwrap();
    assert!(applet.description.contains("wait=true"));
    assert!(!definitions.iter().any(|d| d.name == "user_response"));
    agent.set_form_capabilities(false, true).await;
    assert!(agent.locked_tools.is_some());
    agent.disabled_tools.insert("applet".into());
    agent.invalidate_sdk_tools();
    assert!(
        !agent
            .tool_definitions()
            .await
            .iter()
            .any(|d| d.name == "applet")
    );
    agent.disabled_tools.clear();
    agent.allowed_tools = None;
    agent.set_form_capabilities(true, true).await;
    assert!(
        agent
            .tool_definitions()
            .await
            .iter()
            .any(|d| d.name == "user_response")
    );
    agent.set_form_capabilities(false, false).await;
    assert!(agent.locked_tools.is_none());
    assert!(!agent.tool_names().await.iter().any(|n| n == "applet"));
}
