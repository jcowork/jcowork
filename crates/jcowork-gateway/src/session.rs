//! Session Manager - multi-user session management with DashMap.

use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use jcowork_agent::r#loop::{AgentOutput, UserMessage};
use jcowork_llm::provider::ChatMessage;

/// Handle to a running UserActor.
pub struct UserActorHandle {
    pub user_id: String,
    pub message_tx: mpsc::Sender<UserMessage>,
    pub output_tx: mpsc::Sender<AgentOutput>,
    _handle: JoinHandle<()>,
}

/// A background agent task detached from any single WebSocket connection.
///
/// The task keeps running even if the client disconnects (e.g. the desktop
/// WebView is suspended while the user switches OS apps). All output events
/// are recorded in an ordered log so a reconnecting client can replay what
/// it missed, plus a broadcast channel for live forwarding to the currently
/// attached connection(s).
pub struct RunningTask {
    /// The user message that started this task (conversation fingerprint).
    pub start_message: String,
    /// Ordered log of serialized output events (source of truth for replay).
    events: Mutex<Vec<String>>,
    /// Live event fan-out to attached connections.
    event_tx: broadcast::Sender<String>,
    /// Whether the task is still executing.
    running: AtomicBool,
    /// Abort handle of the spawned task (for the "stop" signal).
    abort_handle: Mutex<Option<tokio::task::AbortHandle>>,
    /// Full conversation history produced when the task finished (system
    /// prompt + messages incl. the completed exchange). Adopted by the next
    /// connection so follow-up messages keep the right context.
    final_history: Mutex<Option<Vec<ChatMessage>>>,
    /// When the task was created (for stale cleanup).
    created_at: std::time::Instant,
}

impl RunningTask {
    pub fn new(start_message: String) -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(4096);
        Arc::new(Self {
            start_message,
            events: Mutex::new(Vec::new()),
            event_tx,
            running: AtomicBool::new(true),
            abort_handle: Mutex::new(None),
            final_history: Mutex::new(None),
            created_at: std::time::Instant::now(),
        })
    }

    /// Record an event: broadcast to live subscribers AND append to the log.
    /// The events lock is held across both operations so that a concurrent
    /// `attach()` (subscribe + snapshot) never loses or duplicates an event.
    pub fn emit(&self, json: String) {
        let _ = self.event_tx.send(json.clone());
        if let Ok(mut events) = self.events.lock() {
            events.push(json);
        }
    }

    /// Atomically subscribe for live events and snapshot the replay log.
    pub fn attach(&self) -> (broadcast::Receiver<String>, Vec<String>) {
        match self.events.lock() {
            Ok(guard) => {
                let rx = self.event_tx.subscribe();
                let snapshot = guard.clone();
                (rx, snapshot)
            }
            Err(_) => (self.event_tx.subscribe(), Vec::new()),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn set_finished(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    pub fn set_abort_handle(&self, handle: tokio::task::AbortHandle) {
        if let Ok(mut guard) = self.abort_handle.lock() {
            *guard = Some(handle);
        }
    }

    /// Abort the task (user pressed "stop"). Returns true if it was running.
    pub fn abort(&self) -> bool {
        if !self.is_running() {
            return false;
        }
        if let Ok(mut guard) = self.abort_handle.lock() {
            if let Some(handle) = guard.take() {
                handle.abort();
            }
        }
        // The spawned task's Drop guard also clears this, but set it now so
        // the flag is consistent even before the runtime processes the abort.
        self.running.store(false, Ordering::SeqCst);
        true
    }

    /// Store the finished conversation history (called once when done).
    pub fn set_final_history(&self, history: Vec<ChatMessage>) {
        if let Ok(mut guard) = self.final_history.lock() {
            *guard = Some(history);
        }
    }

    /// Take the finished history (one-shot adoption by a connection).
    pub fn take_final_history(&self) -> Option<Vec<ChatMessage>> {
        self.final_history.lock().ok().and_then(|mut g| g.take())
    }
}

/// Manages active user sessions across the server.
///
/// Uses DashMap for lock-free concurrent access. Each user gets
/// a UserActor (tokio task) that owns their AgentLoop instance.
pub struct SessionManager {
    actors: DashMap<String, Arc<UserActorHandle>>,
    /// Running (or recently finished) agent tasks keyed by conversation id.
    tasks: DashMap<String, Arc<RunningTask>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            actors: DashMap::new(),
            tasks: DashMap::new(),
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

    /// Get the current (running or recently finished) task of a conversation.
    pub fn get_task(&self, conv: &str) -> Option<Arc<RunningTask>> {
        if conv.is_empty() {
            return None;
        }
        self.tasks.get(conv).map(|r| r.value().clone())
    }

    /// Register a task for a conversation (replaces any previous one),
    /// dropping stale finished tasks older than one hour.
    pub fn set_task(&self, conv: &str, task: Arc<RunningTask>) {
        // Cleanup: remove finished tasks older than 1 hour (bounded memory).
        let cutoff = std::time::Instant::now() - std::time::Duration::from_secs(3600);
        self.tasks.retain(|_, t| t.is_running() || t.created_at > cutoff);
        self.tasks.insert(conv.to_string(), task);
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}
