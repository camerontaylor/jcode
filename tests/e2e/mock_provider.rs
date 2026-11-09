//! Mock provider for e2e tests
//!
//! Returns pre-scripted StreamEvent sequences for deterministic testing.

use anyhow::Result;
use async_stream::stream;
use jcode::message::{Message, StreamEvent, ToolDefinition};
use jcode::provider::{EventStream, Provider};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

pub struct MockProvider {
    responses: Arc<Mutex<VecDeque<Vec<StreamEvent>>>>,
    gate: Option<Arc<tokio::sync::Semaphore>>,
    models: Vec<&'static str>,
    current_model: Arc<Mutex<String>>,
    /// Captured system prompts from complete() calls (for testing)
    pub captured_system_prompts: Arc<Mutex<Vec<String>>>,
    /// Captured resume session IDs from complete() calls (for testing)
    pub captured_resume_session_ids: Arc<Mutex<Vec<Option<String>>>>,
    /// Captured model names from complete() calls (for testing)
    pub captured_models: Arc<Mutex<Vec<String>>>,
    pub captured_tool_names: Arc<Mutex<Vec<Vec<String>>>>,
}

impl MockProvider {
    pub fn new() -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            gate: None,
            models: Vec::new(),
            current_model: Arc::new(Mutex::new("mock".to_string())),
            captured_system_prompts: Arc::new(Mutex::new(Vec::new())),
            captured_resume_session_ids: Arc::new(Mutex::new(Vec::new())),
            captured_models: Arc::new(Mutex::new(Vec::new())),
            captured_tool_names: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Hold each response after its first event until the test releases a permit.
    /// The first event is a wire-visible synchronization point; permits are
    /// consumed, so releasing one turn cannot accidentally release its successor.
    pub fn gated(gate: Arc<tokio::sync::Semaphore>) -> Self {
        Self {
            gate: Some(gate),
            ..Self::new()
        }
    }

    pub fn with_models(models: Vec<&'static str>) -> Self {
        let current = models
            .first()
            .map(|m| (*m).to_string())
            .unwrap_or_else(|| "mock".to_string());
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            gate: None,
            models,
            current_model: Arc::new(Mutex::new(current)),
            captured_system_prompts: Arc::new(Mutex::new(Vec::new())),
            captured_resume_session_ids: Arc::new(Mutex::new(Vec::new())),
            captured_models: Arc::new(Mutex::new(Vec::new())),
            captured_tool_names: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Queue a response (sequence of StreamEvents) to be returned on next complete() call
    pub fn queue_response(&self, events: Vec<StreamEvent>) {
        self.responses.lock().unwrap().push_back(events);
    }
}

#[async_trait::async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        tools: &[ToolDefinition],
        system: &str,
        resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.captured_tool_names
            .lock()
            .unwrap()
            .push(tools.iter().map(|tool| tool.name.clone()).collect());
        // Capture the system prompt for testing
        self.captured_system_prompts
            .lock()
            .unwrap()
            .push(system.to_string());
        self.captured_resume_session_ids
            .lock()
            .unwrap()
            .push(resume_session_id.map(|s| s.to_string()));
        self.captured_models.lock().unwrap().push(self.model());

        let events = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();

        let gate = self.gate.clone();
        let stream = stream! {
            for (index, event) in events.into_iter().enumerate() {
                if index == 1 && let Some(gate) = &gate {
                    gate.acquire().await.expect("test gate remains open").forget();
                }
                yield Ok(event);
            }
        };

        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn model(&self) -> String {
        self.current_model.lock().unwrap().clone()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        if !self.models.is_empty() && !self.models.contains(&model) {
            anyhow::bail!("Unknown model: {}", model);
        }
        *self.current_model.lock().unwrap() = model.to_string();
        Ok(())
    }

    fn available_models(&self) -> Vec<&'static str> {
        self.models.clone()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        let current = self.current_model.lock().unwrap().clone();
        Arc::new(MockProvider {
            responses: self.responses.clone(),
            gate: self.gate.clone(),
            models: self.models.clone(),
            current_model: Arc::new(Mutex::new(current)),
            captured_system_prompts: self.captured_system_prompts.clone(),
            captured_resume_session_ids: self.captured_resume_session_ids.clone(),
            captured_models: self.captured_models.clone(),
            captured_tool_names: self.captured_tool_names.clone(),
        })
    }
}
