//! Ownership of long-lived tasks.
//!
//! Every background task in Litecord is spawned through [`TaskSupervisor`],
//! which records *who owns it* (a name), hands it a [`CancellationToken`] for
//! cooperative shutdown, logs its error on failure, and joins it on
//! [`TaskSupervisor::shutdown`]. No free-floating `tokio::spawn` for
//! long-lived work.

use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::Result;

#[derive(Debug)]
struct Supervised {
    name: &'static str,
    handle: JoinHandle<()>,
}

#[derive(Debug, Default)]
pub struct TaskSupervisor {
    token: CancellationToken,
    tasks: Mutex<Vec<Supervised>>,
}

/// Result of a shutdown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShutdownReport {
    pub joined: Vec<&'static str>,
    /// Tasks that did not stop within the grace period and were aborted.
    pub aborted: Vec<&'static str>,
}

impl TaskSupervisor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Token that is cancelled when shutdown begins.
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    /// Spawn a supervised task. The future receives a child cancellation
    /// token and should return promptly once it is cancelled. Errors are
    /// logged (without secrets) — error *strategy* (retry, restart) belongs
    /// inside the task itself.
    pub fn spawn<F, Fut>(&self, name: &'static str, f: F)
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let fut = f(self.token.child_token());
        let handle = tokio::spawn(async move {
            match fut.await {
                Ok(()) => tracing::debug!(task = name, "task finished"),
                Err(e) => tracing::error!(task = name, error = %e, "task failed"),
            }
        });
        let mut tasks = match self.tasks.lock() {
            Ok(t) => t,
            Err(p) => p.into_inner(),
        };
        tasks.push(Supervised { name, handle });
    }

    pub fn task_names(&self) -> Vec<&'static str> {
        match self.tasks.lock() {
            Ok(t) => t.iter().map(|s| s.name).collect(),
            Err(p) => p.into_inner().iter().map(|s| s.name).collect(),
        }
    }

    /// Cancel all tasks and wait up to `grace` for each to finish.
    pub async fn shutdown(&self, grace: Duration) -> ShutdownReport {
        self.token.cancel();
        let tasks = {
            let mut guard = match self.tasks.lock() {
                Ok(t) => t,
                Err(p) => p.into_inner(),
            };
            std::mem::take(&mut *guard)
        };
        let mut report = ShutdownReport::default();
        for Supervised { name, mut handle } in tasks {
            match tokio::time::timeout(grace, &mut handle).await {
                Ok(_) => report.joined.push(name),
                Err(_) => {
                    handle.abort();
                    tracing::warn!(task = name, "task did not stop in time; aborted");
                    report.aborted.push(name);
                }
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_cancels_and_joins() {
        let sup = TaskSupervisor::new();
        sup.spawn("waiter", |token| async move {
            token.cancelled().await;
            Ok(())
        });
        assert_eq!(sup.task_names(), vec!["waiter"]);
        let report = sup.shutdown(Duration::from_secs(1)).await;
        assert_eq!(report.joined, vec!["waiter"]);
        assert!(report.aborted.is_empty());
    }

    #[tokio::test]
    async fn stuck_tasks_are_aborted() {
        let sup = TaskSupervisor::new();
        sup.spawn("stuck", |_token| async move {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(())
        });
        let report = sup.shutdown(Duration::from_millis(20)).await;
        assert_eq!(report.aborted, vec!["stuck"]);
    }
}
