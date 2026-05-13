//! # DeepOmni Task
//!
//! Durable background task queue with retry and worker pool.
//! Tasks persist their state and support cancellation.
//! Based on DeepSeek-TUI durable task manager pattern.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, Semaphore};
use tracing::{info, warn};

/// Task lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        )
    }
}

/// Retry metadata for a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub attempt: u32,
    pub base_delay_ms: u64,
    pub next_retry_at_ms: Option<u64>,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            attempt: 0,
            base_delay_ms: 500,
            next_retry_at_ms: None,
        }
    }
}

impl RetryConfig {
    pub fn should_retry(&self) -> bool {
        self.attempt < self.max_attempts
    }

    pub fn next_delay_ms(&self) -> u64 {
        self.base_delay_ms * 2u64.pow(self.attempt)
    }
}

/// A task record with its state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub name: String,
    pub status: TaskStatus,
    pub progress: Option<u8>,
    pub detail: Option<String>,
    pub retry: RetryConfig,
    pub created_at: i64,
    pub updated_at: i64,
}

impl TaskRecord {
    pub fn new(name: &str) -> Self {
        Self {
            id: format!("task-{}", uuid::Uuid::new_v4().simple()),
            name: name.to_string(),
            status: TaskStatus::Queued,
            progress: None,
            detail: None,
            retry: RetryConfig::default(),
            created_at: current_timestamp(),
            updated_at: current_timestamp(),
        }
    }
}

/// A task execution function.
pub type TaskFn =
    Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send>;
type PersistFn = Arc<dyn Fn(&TaskRecord) + Send + Sync>;

/// Manages a durable background task queue with retry and worker pool.
pub struct TaskManager {
    tasks: Arc<RwLock<HashMap<String, TaskRecord>>>,
    /// Max concurrent running tasks.
    concurrency_limit: usize,
    semaphore: Arc<Semaphore>,
    /// Callback to persist task state.
    persist_fn: Option<PersistFn>,
}

impl TaskManager {
    pub fn new(concurrency_limit: usize) -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
            concurrency_limit,
            semaphore: Arc::new(Semaphore::new(concurrency_limit)),
            persist_fn: None,
        }
    }

    pub fn concurrency_limit(&self) -> usize {
        self.concurrency_limit
    }

    pub fn with_persistence<F>(mut self, f: F) -> Self
    where
        F: Fn(&TaskRecord) + Send + Sync + 'static,
    {
        self.persist_fn = Some(Arc::new(f));
        self
    }

    /// Enqueue a new task. Returns the task ID.
    pub async fn enqueue(&self, name: &str) -> String {
        let record = TaskRecord::new(name);
        let id = record.id.clone();
        self.tasks.write().await.insert(id.clone(), record);
        id
    }

    /// Spawn a task for execution. Returns immediately; the task runs in background.
    pub async fn spawn<F, Fut>(&self, task_id: &str, f: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), String>> + Send,
    {
        let id = task_id.to_string();
        let tasks = self.tasks.clone();
        let sem = self.semaphore.clone();
        let persist = self.persist_fn.clone();

        // Mark as running.
        {
            let mut guard = tasks.write().await;
            if let Some(record) = guard.get_mut(&id) {
                record.status = TaskStatus::Running;
                record.updated_at = current_timestamp();
                if let Some(ref p) = persist {
                    p(record);
                }
            }
        }

        tokio::spawn(async move {
            let _permit = sem.acquire().await;

            let result = f().await;

            let mut guard = tasks.write().await;
            if let Some(record) = guard.get_mut(&id) {
                record.updated_at = current_timestamp();
                match result {
                    Ok(()) => {
                        record.status = TaskStatus::Completed;
                        record.progress = Some(100);
                        info!(task_id = %id, "task completed");
                    }
                    Err(e) => {
                        record.retry.attempt += 1;
                        if record.retry.should_retry() {
                            record.status = TaskStatus::Queued;
                            record.retry.next_retry_at_ms =
                                Some(current_timestamp_ms() + record.retry.next_delay_ms());
                            record.detail = Some(format!(
                                "retry {}/{}: {e}",
                                record.retry.attempt, record.retry.max_attempts
                            ));
                            warn!(task_id = %id, "task failed, will retry");
                        } else {
                            record.status = TaskStatus::Failed;
                            record.detail = Some(e);
                            warn!(task_id = %id, "task failed permanently");
                        }
                    }
                }
                if let Some(ref p) = persist {
                    p(record);
                }
            }
        });
    }

    /// Cancel a task.
    pub async fn cancel(&self, task_id: &str) -> bool {
        let mut guard = self.tasks.write().await;
        if let Some(record) = guard.get_mut(task_id)
            && !record.status.is_terminal()
        {
            record.status = TaskStatus::Cancelled;
            record.updated_at = current_timestamp();
            if let Some(ref p) = self.persist_fn {
                p(record);
            }
            return true;
        }
        false
    }

    /// Get a task by ID.
    pub async fn get(&self, task_id: &str) -> Option<TaskRecord> {
        self.tasks.read().await.get(task_id).cloned()
    }

    /// List all tasks.
    pub async fn list(&self) -> Vec<TaskRecord> {
        self.tasks.read().await.values().cloned().collect()
    }

    /// List active (non-terminal) tasks.
    pub async fn list_active(&self) -> Vec<TaskRecord> {
        self.tasks
            .read()
            .await
            .values()
            .filter(|t| !t.status.is_terminal())
            .cloned()
            .collect()
    }

    /// Retry failed tasks that are due for retry.
    pub async fn retry_due<F, Fut>(&self, task_id: &str, f: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), String>> + Send,
    {
        let record = self.get(task_id).await;
        if let Some(rec) = record
            && rec.status == TaskStatus::Queued
            && rec.retry.should_retry()
        {
            self.spawn(task_id, f).await;
        }
    }
}

fn current_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_enqueue_and_get() {
        let manager = TaskManager::new(4);
        let id = manager.enqueue("test-task").await;
        let task = manager.get(&id).await.unwrap();
        assert_eq!(task.name, "test-task");
        assert_eq!(task.status, TaskStatus::Queued);
    }

    #[tokio::test]
    async fn test_spawn_and_complete() {
        let manager = Arc::new(TaskManager::new(4));
        let id = manager.enqueue("simple").await;

        manager.spawn(&id, || async { Ok(()) }).await;

        // Give it time to complete.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let task = manager.get(&id).await.unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn test_spawn_and_fail_with_retry() {
        let manager = Arc::new(TaskManager::new(4));
        let id = manager.enqueue("flaky").await;

        manager.spawn(&id, || async { Err("boom".into()) }).await;

        tokio::time::sleep(Duration::from_millis(100)).await;

        let task = manager.get(&id).await.unwrap();
        // First attempt fails, should be queued for retry.
        assert!(task.status == TaskStatus::Queued || task.status == TaskStatus::Failed);
        assert_eq!(task.retry.attempt, 1);
    }

    #[tokio::test]
    async fn test_cancel_task() {
        let manager = TaskManager::new(4);
        let id = manager.enqueue("cancellable").await;
        assert!(manager.cancel(&id).await);

        let task = manager.get(&id).await.unwrap();
        assert_eq!(task.status, TaskStatus::Cancelled);
    }

    #[test]
    fn test_retry_config_backoff() {
        let mut config = RetryConfig::default();
        assert!(config.should_retry());
        config.attempt = 3;
        assert!(!config.should_retry());
        assert_eq!(config.next_delay_ms(), 4000); // 500 * 2^3 = 4000
    }
}
