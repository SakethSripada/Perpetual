//! Resizable admission control with fair queueing.
//!
//! Replaces fixed-size semaphores for session and sandbox concurrency. Unlike
//! a `tokio::sync::Semaphore`, capacity can shrink at runtime (the effective
//! session cap is resource-aware and changes with memory pressure), waiters
//! are served FIFO with skip-over when a per-scope cap blocks the head, and
//! queue depth is observable for capacity snapshots.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

pub(crate) struct AdmissionController {
    inner: Arc<Mutex<State>>,
}

struct State {
    running: usize,
    capacity: usize,
    /// Running count per scope (e.g. project id). Entries are removed at zero.
    per_scope: HashMap<String, usize>,
    /// Optional cap per scope; absent means unbounded within `capacity`.
    scope_caps: HashMap<String, usize>,
    waiters: VecDeque<Waiter>,
    next_waiter_id: u64,
}

struct Waiter {
    id: u64,
    scope: Option<String>,
    tx: oneshot::Sender<AdmissionPermit>,
}

/// A held concurrency slot. Dropping it releases the slot and hands it to the
/// first eligible waiter. Public via the [`crate::SessionPermit`] re-export.
pub struct AdmissionPermit {
    inner: Arc<Mutex<State>>,
    scope: Option<String>,
    armed: bool,
}

impl State {
    fn can_admit(&self, scope: Option<&str>) -> bool {
        if self.running >= self.capacity {
            return false;
        }
        let Some(scope) = scope else { return true };
        match self.scope_caps.get(scope) {
            Some(cap) => self.per_scope.get(scope).copied().unwrap_or(0) < *cap,
            None => true,
        }
    }

    fn admit(&mut self, scope: Option<&str>) {
        self.running += 1;
        if let Some(scope) = scope {
            *self.per_scope.entry(scope.to_string()).or_insert(0) += 1;
        }
    }

    fn release(&mut self, scope: Option<&str>) {
        self.running = self.running.saturating_sub(1);
        if let Some(scope) = scope {
            if let Some(count) = self.per_scope.get_mut(scope) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.per_scope.remove(scope);
                }
            }
        }
    }

    /// Pop the first waiter whose scope allows admission and reserve a slot
    /// for it. FIFO with skip-over: a scope-capped waiter at the head must not
    /// block unrelated work behind it.
    fn pop_and_reserve(&mut self) -> Option<Waiter> {
        if self.running >= self.capacity {
            return None;
        }
        let idx = self
            .waiters
            .iter()
            .position(|w| self.can_admit(w.scope.as_deref()))?;
        let waiter = self.waiters.remove(idx)?;
        self.admit(waiter.scope.as_deref());
        Some(waiter)
    }
}

impl AdmissionController {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(State {
                running: 0,
                capacity: capacity.max(1),
                per_scope: HashMap::new(),
                scope_caps: HashMap::new(),
                waiters: VecDeque::new(),
                next_waiter_id: 0,
            })),
        }
    }

    /// Acquire a slot now, or `None` when at capacity. Interactive starts use
    /// this so the caller can surface a crisp error instead of queueing.
    pub(crate) fn try_acquire(&self, scope: Option<&str>) -> Option<AdmissionPermit> {
        let mut state = self.inner.lock().unwrap();
        if !state.can_admit(scope) {
            return None;
        }
        state.admit(scope);
        drop(state);
        Some(self.permit(scope.map(str::to_string)))
    }

    /// Wait (FIFO, scope-fair) for a slot. Cancel-safe: dropping the future
    /// removes the queue entry, and a permit that raced into the dropped
    /// receiver is released back to the next waiter.
    pub(crate) async fn acquire_queued(&self, scope: Option<&str>) -> AdmissionPermit {
        let (tx, rx) = oneshot::channel();
        let waiter_id = {
            let mut state = self.inner.lock().unwrap();
            if state.can_admit(scope) {
                state.admit(scope);
                drop(state);
                return self.permit(scope.map(str::to_string));
            }
            let id = state.next_waiter_id;
            state.next_waiter_id += 1;
            state.waiters.push_back(Waiter {
                id,
                scope: scope.map(str::to_string),
                tx,
            });
            id
        };
        let guard = WaiterGuard {
            inner: self.inner.clone(),
            id: waiter_id,
            armed: true,
        };
        match rx.await {
            Ok(permit) => {
                let mut guard = guard;
                guard.armed = false;
                permit
            }
            // The controller never drops senders while a waiter is queued, but
            // fail safe by competing for a slot rather than panicking.
            Err(_) => {
                let mut guard = guard;
                guard.armed = false;
                Box::pin(self.acquire_queued(scope)).await
            }
        }
    }

    /// Change total capacity. Growing drains eligible waiters immediately;
    /// shrinking is lazy — running work drains down naturally.
    pub(crate) fn resize(&self, capacity: usize) {
        {
            let mut state = self.inner.lock().unwrap();
            state.capacity = capacity.max(1);
        }
        Self::drain_waiters(&self.inner);
    }

    pub(crate) fn set_scope_cap(&self, scope: &str, cap: Option<usize>) {
        {
            let mut state = self.inner.lock().unwrap();
            match cap {
                Some(cap) => {
                    state.scope_caps.insert(scope.to_string(), cap.max(1));
                }
                None => {
                    state.scope_caps.remove(scope);
                }
            }
        }
        Self::drain_waiters(&self.inner);
    }

    pub(crate) fn capacity(&self) -> usize {
        self.inner.lock().unwrap().capacity
    }

    #[cfg(test)]
    pub(crate) fn running(&self) -> usize {
        self.inner.lock().unwrap().running
    }

    pub(crate) fn queue_len(&self) -> usize {
        self.inner.lock().unwrap().waiters.len()
    }

    fn permit(&self, scope: Option<String>) -> AdmissionPermit {
        AdmissionPermit {
            inner: self.inner.clone(),
            scope,
            armed: true,
        }
    }

    /// Hand freed or newly created slots to waiting acquirers. Sending happens
    /// outside the state lock: a failed send yields a permit whose `Drop`
    /// re-enters this path, which would deadlock under the lock.
    fn drain_waiters(inner: &Arc<Mutex<State>>) {
        loop {
            let waiter = {
                let mut state = inner.lock().unwrap();
                match state.pop_and_reserve() {
                    Some(waiter) => waiter,
                    None => return,
                }
            };
            let scope = waiter.scope.clone();
            let permit = AdmissionPermit {
                inner: inner.clone(),
                scope: waiter.scope,
                armed: true,
            };
            match waiter.tx.send(permit) {
                Ok(()) => continue,
                Err(mut unsent) => {
                    // Receiver dropped (acquire cancelled): take the reserved
                    // slot back without running the permit's Drop.
                    unsent.armed = false;
                    drop(unsent);
                    let mut state = inner.lock().unwrap();
                    state.release(scope.as_deref());
                }
            }
        }
    }

    fn release_slot(inner: &Arc<Mutex<State>>, scope: Option<&str>) {
        {
            let mut state = inner.lock().unwrap();
            state.release(scope);
        }
        Self::drain_waiters(inner);
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        AdmissionController::release_slot(&self.inner, self.scope.take().as_deref());
    }
}

/// Removes an abandoned queue entry when `acquire_queued` is cancelled before
/// its permit arrives.
struct WaiterGuard {
    inner: Arc<Mutex<State>>,
    id: u64,
    armed: bool,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut state = self.inner.lock().unwrap();
        if let Some(idx) = self.waiters_position(&state) {
            state.waiters.remove(idx);
        }
    }
}

impl WaiterGuard {
    fn waiters_position(&self, state: &State) -> Option<usize> {
        state.waiters.iter().position(|w| w.id == self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn caps_and_frees_on_drop() {
        let ctl = AdmissionController::new(2);
        let p1 = ctl.try_acquire(None).expect("first");
        let _p2 = ctl.try_acquire(None).expect("second");
        assert!(ctl.try_acquire(None).is_none());
        drop(p1);
        assert!(ctl.try_acquire(None).is_some());
    }

    #[tokio::test]
    async fn queued_waiters_are_fifo() {
        let ctl = Arc::new(AdmissionController::new(1));
        let first = ctl.try_acquire(None).expect("slot");

        let (order_tx, mut order_rx) = tokio::sync::mpsc::unbounded_channel();
        for label in ["a", "b", "c"] {
            let task_ctl = ctl.clone();
            let order_tx = order_tx.clone();
            tokio::spawn(async move {
                let permit = task_ctl.acquire_queued(None).await;
                order_tx.send(label).unwrap();
                drop(permit);
            });
            // Deterministic enqueue order.
            while ctl.queue_len() < 1 {
                tokio::task::yield_now().await;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        drop(first);
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(order_rx.recv().await.unwrap());
        }
        assert_eq!(seen, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn scope_cap_skips_over_blocked_head() {
        let ctl = Arc::new(AdmissionController::new(4));
        ctl.set_scope_cap("p1", Some(1));
        let held = ctl.try_acquire(Some("p1")).expect("p1 slot");
        // Saturate total capacity so both waiters queue.
        let f1 = ctl.try_acquire(None).unwrap();
        let _f2 = ctl.try_acquire(None).unwrap();
        let _f3 = ctl.try_acquire(None).unwrap();

        let blocked = {
            let ctl = ctl.clone();
            tokio::spawn(async move { ctl.acquire_queued(Some("p1")).await })
        };
        while ctl.queue_len() < 1 {
            tokio::task::yield_now().await;
        }
        let unblocked = {
            let ctl = ctl.clone();
            tokio::spawn(async move { ctl.acquire_queued(Some("p2")).await })
        };
        while ctl.queue_len() < 2 {
            tokio::task::yield_now().await;
        }

        // Free one general slot: the head waiter (p1) is still scope-blocked,
        // so the p2 waiter behind it must be admitted instead.
        drop(f1);
        let p2_permit = tokio::time::timeout(Duration::from_secs(1), unblocked)
            .await
            .expect("p2 admitted past blocked head")
            .unwrap();
        assert!(!blocked.is_finished());

        // Releasing the p1 slot admits the scoped waiter.
        drop(held);
        let _p1_permit = tokio::time::timeout(Duration::from_secs(1), blocked)
            .await
            .expect("p1 admitted after scope freed")
            .unwrap();
        drop(p2_permit);
    }

    #[tokio::test]
    async fn resize_grow_drains_waiters_and_shrink_is_lazy() {
        let ctl = Arc::new(AdmissionController::new(1));
        let held = ctl.try_acquire(None).expect("slot");
        let waiter = {
            let ctl = ctl.clone();
            tokio::spawn(async move { ctl.acquire_queued(None).await })
        };
        while ctl.queue_len() < 1 {
            tokio::task::yield_now().await;
        }

        ctl.resize(2);
        let extra = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("grow admits waiter")
            .unwrap();

        // Shrink below running: nothing is evicted, but no new admissions.
        ctl.resize(1);
        assert_eq!(ctl.running(), 2);
        assert!(ctl.try_acquire(None).is_none());
        drop(extra);
        assert!(ctl.try_acquire(None).is_none(), "still above shrunk cap");
        drop(held);
        assert!(ctl.try_acquire(None).is_some());
    }

    #[tokio::test]
    async fn cancelled_waiter_does_not_leak_slot_or_position() {
        let ctl = Arc::new(AdmissionController::new(1));
        let held = ctl.try_acquire(None).expect("slot");

        let cancelled = {
            let ctl = ctl.clone();
            tokio::spawn(async move { ctl.acquire_queued(None).await })
        };
        while ctl.queue_len() < 1 {
            tokio::task::yield_now().await;
        }
        cancelled.abort();
        let _ = cancelled.await;

        let survivor = {
            let ctl = ctl.clone();
            tokio::spawn(async move { ctl.acquire_queued(None).await })
        };
        drop(held);
        let permit = tokio::time::timeout(Duration::from_secs(1), survivor)
            .await
            .expect("slot reaches surviving waiter")
            .unwrap();
        drop(permit);
        assert_eq!(ctl.running(), 0);
        assert_eq!(ctl.queue_len(), 0);
    }
}
