#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::{
    SessionInterruptQueues, queue_soft_interrupt_for_session, register_session_interrupt_queue,
};
use crate::agent::Agent;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use anyhow::Result;
use async_trait::async_trait;
use jcode_agent_runtime::SoftInterruptSource;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

struct TestProvider;

#[async_trait]
impl Provider for TestProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Err(anyhow::anyhow!(
            "test provider complete should not be called in queue tests"
        ))
    }

    fn name(&self) -> &str {
        "test"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(TestProvider)
    }
}

async fn test_agent() -> Arc<Mutex<Agent>> {
    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let registry = Registry::new(provider.clone()).await;
    Arc::new(Mutex::new(Agent::new(provider, registry)))
}

#[tokio::test]
async fn queue_soft_interrupt_for_session_uses_registered_queue_when_agent_busy() {
    let agent = test_agent().await;
    let session_id = {
        let guard = agent.lock().await;
        guard.session_id().to_string()
    };
    let queue = {
        let guard = agent.lock().await;
        guard.soft_interrupt_queue()
    };
    let queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    register_session_interrupt_queue(&queues, &session_id, queue.clone()).await;
    let sessions = Arc::new(crate::server::session_manager::SessionManager::from_agents(
        HashMap::from([(session_id.clone(), agent.clone())]),
    ));

    let _busy_guard = agent.lock().await;
    let queued = queue_soft_interrupt_for_session(
        &session_id,
        "queued while busy".to_string(),
        false,
        SoftInterruptSource::User,
        &queues,
        &sessions,
    )
    .await;

    assert!(
        queued,
        "interrupt should queue even while agent lock is held"
    );
    let pending = queue.lock().expect("queue lock");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].content, "queued while busy");
    assert!(!pending[0].urgent);
    assert_eq!(pending[0].source, SoftInterruptSource::User);
}

#[tokio::test]
async fn queue_soft_interrupt_for_session_registers_queue_on_fallback_lookup() {
    let agent = test_agent().await;
    let session_id = {
        let guard = agent.lock().await;
        guard.session_id().to_string()
    };
    let queue = {
        let guard = agent.lock().await;
        guard.soft_interrupt_queue()
    };
    let queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let sessions = Arc::new(crate::server::session_manager::SessionManager::from_agents(
        HashMap::from([(session_id.clone(), agent.clone())]),
    ));

    let queued = queue_soft_interrupt_for_session(
        &session_id,
        "fallback lookup".to_string(),
        true,
        SoftInterruptSource::System,
        &queues,
        &sessions,
    )
    .await;

    assert!(queued, "interrupt should queue via session fallback");
    assert!(
        queues.read().await.contains_key(&session_id),
        "fallback should cache the session queue for later busy sends"
    );
    let pending = queue.lock().expect("queue lock");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].content, "fallback lookup");
    assert!(pending[0].urgent);
    assert_eq!(pending[0].source, SoftInterruptSource::System);
}

#[tokio::test]
async fn queue_soft_interrupt_for_session_persists_when_live_queue_is_unavailable() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().expect("temp dir");
    let prev_home = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());

    let agent = test_agent().await;
    let session_id = {
        let guard = agent.lock().await;
        guard.session_id().to_string()
    };
    crate::session::Session::create_with_id(session_id.clone(), None, None)
        .save_prepared()
        .expect("save session snapshot");

    let queues: SessionInterruptQueues = Arc::new(RwLock::new(HashMap::new()));
    let sessions = Arc::new(crate::server::session_manager::SessionManager::from_agents(
        HashMap::new(),
    ));

    let queued = queue_soft_interrupt_for_session(
        &session_id,
        "persist while reloading".to_string(),
        false,
        SoftInterruptSource::BackgroundTask,
        &queues,
        &sessions,
    )
    .await;

    assert!(
        queued,
        "interrupt should persist when live queue is unavailable"
    );

    let persisted =
        crate::soft_interrupt_store::load(&session_id).expect("load persisted interrupts");
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].content, "persist while reloading");
    assert_eq!(persisted[0].source, SoftInterruptSource::BackgroundTask);

    let provider: Arc<dyn Provider> = Arc::new(TestProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut restored = Agent::new(provider, registry);
    restored
        .restore_session(&session_id)
        .expect("restore session should rehydrate interrupts");
    assert_eq!(restored.soft_interrupt_count(), 1);
    assert!(
        crate::soft_interrupt_store::load(&session_id)
            .expect("load persisted interrupts after restore")
            .is_empty()
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn transport_loss_withdraws_only_owned_queued_input() {
    use super::state::SessionControlHandle;
    use jcode_agent_runtime::{InterruptSignal, SoftInterruptMessage};
    let _env = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let previous = std::env::var_os("JCODE_HOME");
    crate::env::set_var("JCODE_HOME", temp.path());
    let queue = Arc::new(std::sync::Mutex::new(Vec::new()));
    let control = SessionControlHandle::cancel_only("owned", queue.clone(), InterruptSignal::new());
    let input = |owner: &str, content: &str| SoftInterruptMessage {
        content: content.into(),
        images: vec![],
        urgent: false,
        source: SoftInterruptSource::User,
        client_id: Some("reused-opaque-id".into()),
        transport_owner: Some(owner.into()),
    };
    assert!(control.queue_transport_input(input("closed", "already injected")));
    let injected = queue.lock().unwrap().remove(0);
    assert!(control.queue_transport_input(input("closed", "withdraw")));
    assert!(control.queue_transport_input(input("live", "preserve")));
    crate::soft_interrupt_store::overwrite("owned", &queue.lock().unwrap()).unwrap();
    let other_queue = Arc::new(std::sync::Mutex::new(vec![input(
        "closed",
        "other session",
    )]));
    let other =
        SessionControlHandle::cancel_only("other", other_queue.clone(), InterruptSignal::new());
    assert!(!control.shares_pending_queue(&other));
    assert!(control.shares_pending_queue(&control.clone()));
    // Captured control remains correct even if the connection now targets another session.
    let captured = control.clone();
    captured.withdraw_transport_input("closed");
    assert_eq!(injected.content, "already injected");
    assert_eq!(queue.lock().unwrap()[0].content, "preserve");
    assert_eq!(queue.lock().unwrap().len(), 1);
    assert_eq!(other_queue.lock().unwrap()[0].content, "other session");
    let disk = crate::soft_interrupt_store::load("owned").unwrap();
    assert_eq!(disk.len(), 1);
    assert_eq!(disk[0].transport_owner.as_deref(), Some("live"));
    // Reconnection has a new daemon-generated owner, even with identical caller IDs.
    assert!(control.queue_transport_input(input("reconnected", "new transport")));
    captured.withdraw_transport_input("closed");
    assert_eq!(queue.lock().unwrap().len(), 2);
    if let Some(previous) = previous {
        crate::env::set_var("JCODE_HOME", previous);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn rejected_transport_input_is_not_admitted() {
    use super::state::SessionControlHandle;
    use jcode_agent_runtime::{InterruptSignal, SoftInterruptMessage};
    let queue = Arc::new(std::sync::Mutex::new(Vec::new()));
    let poisoned = queue.clone();
    let _ = std::thread::spawn(move || {
        let _guard = poisoned.lock().unwrap();
        panic!("poison queue");
    })
    .join();
    let control =
        SessionControlHandle::cancel_only("rejected", queue.clone(), InterruptSignal::new());
    assert!(!control.queue_transport_input(SoftInterruptMessage {
        content: "rejected".into(),
        images: vec![],
        urgent: false,
        source: SoftInterruptSource::User,
        client_id: None,
        transport_owner: Some("closed".into()),
    }));
    assert!(queue.lock().unwrap_err().into_inner().is_empty());
}
