//! ACP model and reasoning effort configuration presentation.

use super::*;

/// Catalog-only History can omit capabilities while the provider is busy.
pub(super) fn apply_catalog_state(state: &mut SessionUiState, event: &ServerEvent) {
    if let ServerEvent::History {
        provider_name,
        provider_model,
        available_models,
        reasoning_effort,
        available_reasoning_efforts,
        ..
    } = event
    {
        let previous_provider = state.provider_name.clone();
        let previous_model = state.model.clone();
        if provider_name.is_some() {
            state.provider_name = provider_name.clone();
        }
        if provider_model.is_some() {
            state.model = provider_model.clone();
        }
        state.available_models = available_models.clone();
        state.reasoning_effort = reasoning_effort.clone();
        if available_reasoning_efforts.is_some()
            || previous_provider != state.provider_name
            || previous_model != state.model
        {
            state.available_reasoning_efforts = available_reasoning_efforts.clone();
        }
    }
}

pub(super) const CONFIG_ID_MODEL: &str = "model";
pub(super) const CONFIG_ID_EFFORT: &str = "reasoning_effort";

pub(super) fn acp_available_commands() -> Vec<Value> {
    vec![
        json!({
            "name": "model",
            "description": "Switch the model for this session, or show the current model",
            "input": { "hint": "model id (optional)" },
        }),
        json!({
            "name": "models",
            "description": "List models available from the active provider",
        }),
        json!({
            "name": "effort",
            "description": "Set reasoning effort, or show the current effort",
            "input": { "hint": "none|minimal|low|medium|high|xhigh|max (optional)" },
        }),
        json!({
            "name": "compact",
            "description": "Summarize older messages to free context (applied on the next prompt)",
        }),
        json!({
            "name": "subagent",
            "description": "Launch a background swarm worker with a prompt",
            "input": { "hint": "[--model <id>] [--type <kind>] <prompt>" },
        }),
    ]
}

pub(super) fn insert_session_configuration(result: &mut Value, state: &SessionUiState) {
    let Some(object) = result.as_object_mut() else {
        return;
    };
    let config_options = session_config_options(state);
    if !config_options.is_empty() {
        object.insert("configOptions".to_string(), Value::Array(config_options));
    }
    if let Some(models) = session_models(state) {
        object.insert("models".to_string(), models);
    }
}

pub(super) fn session_models(state: &SessionUiState) -> Option<Value> {
    let current = state.model.as_deref()?;
    let mut models = state.available_models.clone();
    if !models.iter().any(|candidate| candidate == current) {
        models.insert(0, current.to_string());
    }
    Some(json!({
        "availableModels": models
            .into_iter()
            .map(|model| json!({ "modelId": model, "name": model }))
            .collect::<Vec<_>>(),
        "currentModelId": current,
    }))
}

pub(super) fn available_efforts(state: &SessionUiState) -> Vec<&str> {
    let efforts = match &state.available_reasoning_efforts {
        Some(efforts) => efforts.iter().map(String::as_str).collect(),
        None => crate::provider::inferred_reasoning_efforts(
            state.provider_name.as_deref(),
            state.model.as_deref(),
        ),
    };
    efforts
        .into_iter()
        .filter(|effort| crate::provider::canonical_reasoning_effort(effort).is_some())
        .collect()
}

/// An unsupported effort is a harmless no-op; valid levels are clamped to the
/// nearest advertised level before contacting the daemon.
pub(super) fn normalize_effort(state: &SessionUiState, requested: &str) -> Result<Option<String>> {
    if matches!(
        requested.trim().to_ascii_lowercase().as_str(),
        "default" | "auto"
    ) {
        return Ok(Some(requested.trim().to_ascii_lowercase()));
    }
    let efforts = available_efforts(state);
    if efforts.is_empty() {
        let warning = format!(
            "Ignoring reasoning effort '{requested}': model '{}' does not support it",
            state.model.as_deref().unwrap_or("unknown")
        );
        crate::logging::warn(&warning);
        eprintln!("Warning: {warning}");
        return Ok(None);
    }
    let selected = crate::provider::closest_reasoning_effort(requested, &efforts)
        .ok_or_else(|| anyhow::anyhow!("Unknown reasoning effort: {requested}"))?;
    if !selected.eq_ignore_ascii_case(requested.trim()) {
        eprintln!("Warning: reasoning effort '{requested}' is unavailable; using '{selected}'");
    }
    Ok(Some(selected.to_string()))
}

/// Build the ACP `configOptions` array (model selector plus reasoning effort)
/// from the current session provider state. Empty when the daemon reported no
/// usable model state.
pub(super) fn session_config_options(state: &SessionUiState) -> Vec<Value> {
    let mut options = Vec::new();

    if let Some(model) = state.model.as_deref() {
        let mut models = state.available_models.clone();
        if !models.iter().any(|candidate| candidate == model) {
            models.insert(0, model.to_string());
        }
        let select_options: Vec<Value> = models
            .iter()
            .map(|name| json!({ "value": name, "name": name }))
            .collect();
        options.push(json!({
            "type": "select",
            "id": CONFIG_ID_MODEL,
            "name": "Model",
            "category": "model",
            "currentValue": model,
            "options": select_options,
        }));
    }

    let efforts = available_efforts(state);
    if !efforts.is_empty() {
        let current = crate::provider::closest_reasoning_effort(
            state.reasoning_effort.as_deref().unwrap_or("medium"),
            &efforts,
        )
        .or_else(|| crate::provider::closest_reasoning_effort("medium", &efforts))
        .unwrap_or(efforts[0]);
        let select_options: Vec<Value> = efforts
            .iter()
            .map(|name| json!({ "value": name, "name": name }))
            .collect();
        options.push(json!({
            "type": "select",
            "id": CONFIG_ID_EFFORT,
            "name": "Reasoning effort",
            "category": "thought_level",
            "currentValue": current,
            "options": select_options,
        }));
    }

    options
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_omissions_preserve_capabilities_only_for_unchanged_model() {
        let mut state = SessionUiState {
            provider_name: Some("copilot".into()),
            model: Some("claude-sonnet-5".into()),
            available_reasoning_efforts: Some(vec!["high".into()]),
            ..Default::default()
        };
        let catalog = |model: &str, efforts: Value| {
            serde_json::from_value::<ServerEvent>(json!({
                "type": "history", "id": 5, "session_id": "s", "messages": [],
                "provider_name": "copilot", "provider_model": model,
                "available_reasoning_efforts": efforts
            }))
            .unwrap()
        };
        apply_catalog_state(&mut state, &catalog("claude-sonnet-5", Value::Null));
        assert_eq!(available_efforts(&state), vec!["high"]);
        apply_catalog_state(&mut state, &catalog("another-model", Value::Null));
        assert!(state.available_reasoning_efforts.is_none());
        apply_catalog_state(&mut state, &catalog("another-model", json!([])));
        assert_eq!(state.available_reasoning_efforts, Some(vec![]));
    }

    #[test]
    fn provider_default_resets_bypass_ranked_normalization() {
        let state = SessionUiState {
            available_reasoning_efforts: Some(vec![]),
            ..Default::default()
        };
        for reset in ["default", "auto"] {
            assert_eq!(
                normalize_effort(&state, reset).unwrap().as_deref(),
                Some(reset)
            );
        }
    }

    #[test]
    fn authoritative_capabilities_override_name_inference() {
        let state = SessionUiState {
            provider_name: Some("openai-compatible:cerebras".into()),
            model: Some("gpt-oss-120b".into()),
            available_reasoning_efforts: Some(vec![]),
            ..Default::default()
        };
        let options = session_config_options(&state);
        assert_eq!(options.len(), 1);
        assert_eq!(options[0]["id"], CONFIG_ID_MODEL);
        assert_eq!(normalize_effort(&state, "high").unwrap(), None);
    }

    #[test]
    fn tui_sentinel_uses_a_supported_default_in_config() {
        let state = SessionUiState {
            model: Some("reasoning-model".into()),
            reasoning_effort: Some("swarm".into()),
            available_reasoning_efforts: Some(vec!["low".into(), "medium".into(), "high".into()]),
            ..Default::default()
        };
        assert_eq!(session_config_options(&state)[1]["currentValue"], "medium");
    }

    #[test]
    fn model_ladder_controls_options_and_normalizes_requests() {
        let state = SessionUiState {
            provider_name: Some("openai".into()),
            model: Some("gpt-5-pro".into()),
            reasoning_effort: Some("high".into()),
            available_reasoning_efforts: Some(vec!["high".into(), "swarm".into()]),
            ..Default::default()
        };
        let options = session_config_options(&state);
        assert_eq!(
            options[1]["options"],
            json!([{"value":"high","name":"high"}])
        );
        assert_eq!(
            normalize_effort(&state, "max").unwrap().as_deref(),
            Some("high")
        );
        assert_eq!(
            normalize_effort(&state, "none").unwrap().as_deref(),
            Some("high")
        );
        assert!(normalize_effort(&state, "swarm").is_err());
    }
}
