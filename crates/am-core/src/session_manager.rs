use std::collections::HashMap;

use am_agents::SessionControl;
use tokio::sync::Mutex;

use crate::admission::{AdmissionController, AdmissionPermit};

/// A held session slot; dropping it frees the slot for the next waiter.
pub type SessionPermit = AdmissionPermit;

/// Tracks running sessions and caps concurrency. One active session per task.
///
/// Concurrency is bounded by a resizable [`AdmissionController`]: the
/// resource-aware effective cap is applied via [`SessionManager::resize`], so
/// capacity follows memory pressure instead of being fixed at startup.
/// Interactive starts use [`SessionManager::try_acquire`] (crisp error at
/// capacity); background work (scheduler, plan driver) uses
/// [`SessionManager::acquire_queued`] and waits fairly instead of racing
/// retryable errors. Stopping or app shutdown cancels controls, whose drivers
/// kill the process group — so we never leak processes or oversubscribe
/// CPU/RAM.
pub struct SessionManager {
    active: Mutex<HashMap<String, SessionControl>>,
    admission: AdmissionController,
}

impl SessionManager {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            active: Mutex::new(HashMap::new()),
            admission: AdmissionController::new(max_concurrent),
        }
    }

    pub async fn is_active(&self, task_id: &str) -> bool {
        self.active.lock().await.contains_key(task_id)
    }

    /// Acquire a concurrency slot now, or `Err` if at capacity. `scope` is the
    /// project id when known, so per-project caps apply.
    pub fn try_acquire(&self, scope: Option<&str>) -> Result<SessionPermit, ()> {
        self.admission.try_acquire(scope).ok_or(())
    }

    /// Wait for a concurrency slot (FIFO, scope-fair). Cancel-safe.
    pub async fn acquire_queued(&self, scope: Option<&str>) -> SessionPermit {
        self.admission.acquire_queued(scope).await
    }

    /// Apply a new effective capacity. Growing admits queued waiters
    /// immediately; shrinking lets running sessions drain naturally.
    pub fn resize(&self, capacity: usize) {
        self.admission.resize(capacity);
    }

    /// Cap concurrent sessions for one project (`None` removes the cap).
    pub fn set_project_cap(&self, project_id: &str, cap: Option<usize>) {
        self.admission.set_scope_cap(project_id, cap);
    }

    pub fn capacity(&self) -> usize {
        self.admission.capacity()
    }

    pub fn queue_len(&self) -> usize {
        self.admission.queue_len()
    }

    pub async fn active_count(&self) -> usize {
        self.active.lock().await.len()
    }

    pub async fn register(&self, task_id: &str, control: SessionControl) {
        self.active
            .lock()
            .await
            .insert(task_id.to_string(), control);
    }

    /// Cancel a running session (kills its process group via the driver).
    pub async fn cancel(&self, task_id: &str) -> bool {
        if let Some(mut control) = self.active.lock().await.remove(task_id) {
            control.cancel();
            true
        } else {
            false
        }
    }

    pub async fn remove(&self, task_id: &str) {
        self.active.lock().await.remove(task_id);
    }

    /// Cancel every running session (used on app shutdown).
    pub async fn shutdown(&self) {
        let mut map = self.active.lock().await;
        for (_, mut control) in map.drain() {
            control.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn caps_concurrency_and_frees_on_drop() {
        let mgr = SessionManager::new(2);
        let p1 = mgr.try_acquire(None).expect("first slot");
        let _p2 = mgr.try_acquire(None).expect("second slot");
        assert!(
            mgr.try_acquire(None).is_err(),
            "third should be at capacity"
        );
        drop(p1);
        assert!(mgr.try_acquire(None).is_ok(), "slot freed after drop");
    }

    #[tokio::test]
    async fn queued_acquire_waits_for_release() {
        let mgr = std::sync::Arc::new(SessionManager::new(1));
        let held = mgr.try_acquire(None).expect("slot");
        let waiter = {
            let mgr = mgr.clone();
            tokio::spawn(async move { mgr.acquire_queued(None).await })
        };
        while mgr.queue_len() < 1 {
            tokio::task::yield_now().await;
        }
        assert!(!waiter.is_finished());
        drop(held);
        let permit = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("queued acquire admitted after release")
            .unwrap();
        drop(permit);
    }

    #[tokio::test]
    async fn resize_updates_capacity() {
        let mgr = SessionManager::new(1);
        let _held = mgr.try_acquire(None).expect("slot");
        assert!(mgr.try_acquire(None).is_err());
        mgr.resize(2);
        assert_eq!(mgr.capacity(), 2);
        assert!(mgr.try_acquire(None).is_ok());
    }

    #[tokio::test]
    async fn register_cancel_remove_lifecycle() {
        let mgr = SessionManager::new(4);
        assert!(!mgr.is_active("t1").await);

        let (tx, mut rx) = oneshot::channel();
        mgr.register("t1", SessionControl::new(tx)).await;
        assert!(mgr.is_active("t1").await);

        // Cancelling delivers the signal (the driver would kill the process group)
        // and reports that a session was removed.
        assert!(mgr.cancel("t1").await);
        assert!(rx.try_recv().is_ok(), "cancel signal delivered");
        assert!(!mgr.is_active("t1").await);

        // Cancelling an unknown task is a no-op.
        assert!(!mgr.cancel("missing").await);
    }

    #[tokio::test]
    async fn shutdown_cancels_every_session() {
        let mgr = SessionManager::new(4);
        let (tx_a, mut rx_a) = oneshot::channel();
        let (tx_b, mut rx_b) = oneshot::channel();
        mgr.register("a", SessionControl::new(tx_a)).await;
        mgr.register("b", SessionControl::new(tx_b)).await;

        mgr.shutdown().await;

        assert!(!mgr.is_active("a").await);
        assert!(!mgr.is_active("b").await);
        assert!(rx_a.try_recv().is_ok());
        assert!(rx_b.try_recv().is_ok());
    }
}
