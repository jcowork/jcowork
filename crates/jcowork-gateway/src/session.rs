//! Session Manager - multi-user session management with DashMap.

use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use jcowork_agent::r#loop::{AgentOutput, UserMessage};

/// Handle to a running UserActor.
pub struct UserActorHandle {
    pub user_id: String,
    pub message_tx: mpsc::Sender<UserMessage>,
    pub output_tx: mpsc::Sender<AgentOutput>,
    _handle: JoinHandle<()>,
}

/// Manages active user sessions across the server.
///
/// Uses DashMap for lock-free concurrent access. Each user gets
/// a UserActor (tokio task) that owns their AgentLoop instance.
pub struct SessionManager {
    actors: DashMap<String, Arc<UserActorHandle>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            actors: DashMap::new(),
        }
    }

    /// Register a new user actor.
    pub fn insert(&self, handle: UserActorHandle) {
        let user_id = handle.user_id.clone();
        self.actors.insert(user_id, Arc::new(handle));
    }

    /// Get a user actor handle.
    pub fn get(&self, user_id: &str) -> Option<Arc<UserActorHandle>> {
        self.actors.get(user_id).map(|r| r.value().clone())
    }

    /// Remove a user actor.
    pub fn remove(&self, user_id: &str) -> Option<Arc<UserActorHandle>> {
        self.actors.remove(user_id).map(|(_, v)| v)
    }

    /// Get count of active sessions.
    pub fn active_count(&self) -> usize {
        self.actors.len()
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}
