//! Delegate - spawn sub-agent tasks for parallel work.

use anyhow::Result;
use tokio::sync::oneshot;

/// A delegate task that runs a sub-agent independently.
pub struct DelegateTask {
    pub task_description: String,
    pub workspace: String,
}

/// Handle to a running sub-agent.
pub struct DelegateHandle {
    pub task_id: String,
    rx: oneshot::Receiver<Result<String>>,
}

impl DelegateHandle {
    /// Wait for the sub-agent to complete and return the result.
    pub async fn join(self) -> Result<String> {
        self.rx.await.map_err(|_| anyhow::anyhow!("Sub-agent crashed"))?
    }
}

/// Spawns sub-agent tasks for parallel execution.
///
/// Sub-agents run as independent tokio tasks.
/// tokio tasks with their own agent loop instance.
pub struct DelegateSpawner {
    #[allow(dead_code)]
    user_id: String,
    #[allow(dead_code)]
    workspace_root: String,
}

impl DelegateSpawner {
    pub fn new(user_id: String, workspace_root: String) -> Self {
        Self { user_id, workspace_root }
    }

    /// Spawn a sub-agent task.
    ///
    /// In a full implementation, this would create a new AgentLoop instance
    /// with a reduced toolset and run it to completion in a tokio task.
    /// For now, it returns a placeholder.
    pub fn spawn(&self, task: DelegateTask) -> DelegateHandle {
        let (tx, rx) = oneshot::channel();
        let task_id = uuid::Uuid::new_v4().to_string();
        let _task_id_clone = task_id.clone();

        tokio::spawn(async move {
            // In production: create a new AgentLoop, run it with the task description,
            // and send the result back via tx.
            let result = Ok(format!(
                "Sub-agent completed task: {}",
                task.task_description
            ));
            let _ = tx.send(result);
        });

        DelegateHandle { task_id, rx }
    }
}
