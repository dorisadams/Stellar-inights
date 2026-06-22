//! Backend sync queue.
//!
//! Companion to `mobile/src/services/database.ts`'s `sync_queue` table.
//! Messages produced offline are replayed here; each message MUST be
//! processed **idempotently** so re-deliveries from the mobile client are
//! safe.
//!
//! See `docs/offline-sync.md` for the full replay protocol.
//!
//! Issue #93 acceptance criteria:
//! * Replayed offline actions are processed safely and idempotently.
//! * Retry behaviour is covered by `tests::replay_is_idempotent`.

pub mod replay;
pub mod types;

pub use replay::{QueueProcessor, QueueReplayError};
pub use types::{QueueMessage, QueueMessageStatus, QueueMessageType};
//! Idempotent offline-action replay queue.
//!
//! Mobile and frontend clients persist mutations locally while offline (see
//! `mobile/src/services/database.ts`'s `sync_queue` table and the replay
//! logic in `mobile/src/hooks/useOfflineCaching.ts`) and resubmit them once
//! connectivity returns. A replay can be interrupted and retried, so every
//! action carries a client-generated `id` that this queue treats as an
//! idempotency key: applying the same id twice runs the underlying mutation
//! at most once, even under concurrent replay (e.g. two devices, or a retry
//! racing the original request).
//!
//! See `docs/offline-sync.md` for the full client/server reconciliation
//! contract this module implements one half of.

use chrono::{DateTime, Utc};
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum OfflineActionMethod {
    Post,
    Put,
    Delete,
}

/// A single offline mutation submitted for replay, mirroring the shape
/// persisted client-side in `sync_queue` rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfflineAction {
    /// Client-generated idempotency key, stable across retries of the same logical action.
    pub id: String,
    pub method: OfflineActionMethod,
    pub resource: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    pub client_timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfflineActionOutcome {
    /// First time this id has been seen; the action was applied.
    Applied,
    /// This id was already applied (or is being applied concurrently); not re-applied.
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessedState {
    /// Another caller has claimed this id and is currently applying it.
    Reserved,
    /// The action was applied successfully.
    Applied,
}

#[derive(Debug, Clone)]
struct ProcessedRecord {
    state: ProcessedState,
    #[allow(dead_code)] // surfaced via future admin/debug endpoints; kept for observability now
    processed_at: DateTime<Utc>,
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct OfflineSyncQueueMetrics {
    pub received: u64,
    pub applied: u64,
    pub duplicates: u64,
    pub failed: u64,
}

/// Idempotent, concurrency-safe queue for replaying offline mobile/frontend
/// mutations against the backend.
///
/// This is a library-level primitive: it does not perform the mutation
/// itself. Callers pass an `apply` closure (e.g. dispatching to the relevant
/// resource handler) and [`OfflineSyncQueue::submit`] guarantees that closure
/// runs at most once per action id.
#[derive(Clone)]
pub struct OfflineSyncQueue {
    processed: Arc<DashMap<String, ProcessedRecord>>,
    received: Arc<AtomicU64>,
    applied: Arc<AtomicU64>,
    duplicates: Arc<AtomicU64>,
    failed: Arc<AtomicU64>,
}

impl Default for OfflineSyncQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl OfflineSyncQueue {
    pub fn new() -> Self {
        Self {
            processed: Arc::new(DashMap::new()),
            received: Arc::new(AtomicU64::new(0)),
            applied: Arc::new(AtomicU64::new(0)),
            duplicates: Arc::new(AtomicU64::new(0)),
            failed: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Submits an offline action for application, running `apply` at most
    /// once per `action.id`.
    ///
    /// The id is reserved atomically before `apply` runs, so two callers
    /// racing to submit the same id concurrently cannot both apply it: the
    /// loser observes `Duplicate` immediately rather than waiting on or
    /// re-running the winner's mutation. If `apply` fails, the reservation
    /// is released so a later resubmission of the same id can retry.
    pub async fn submit<F, Fut>(
        &self,
        action: OfflineAction,
        apply: F,
    ) -> anyhow::Result<OfflineActionOutcome>
    where
        F: FnOnce(OfflineAction) -> Fut,
        Fut: Future<Output = anyhow::Result<()>>,
    {
        self.received.fetch_add(1, Ordering::Relaxed);

        match self.processed.entry(action.id.clone()) {
            Entry::Occupied(_) => {
                self.duplicates.fetch_add(1, Ordering::Relaxed);
                info!(action_id = %action.id, "Rejected duplicate offline action replay");
                return Ok(OfflineActionOutcome::Duplicate);
            }
            Entry::Vacant(vacant) => {
                vacant.insert(ProcessedRecord {
                    state: ProcessedState::Reserved,
                    processed_at: Utc::now(),
                });
            }
        }

        match apply(action.clone()).await {
            Ok(()) => {
                self.processed.insert(
                    action.id.clone(),
                    ProcessedRecord {
                        state: ProcessedState::Applied,
                        processed_at: Utc::now(),
                    },
                );
                self.applied.fetch_add(1, Ordering::Relaxed);
                Ok(OfflineActionOutcome::Applied)
            }
            Err(error) => {
                // Release the reservation so the same id can be retried.
                self.processed.remove(&action.id);
                self.failed.fetch_add(1, Ordering::Relaxed);
                warn!(action_id = %action.id, %error, "Failed to apply offline action");
                Err(error)
            }
        }
    }

    /// True if `id` has already been successfully applied.
    pub fn has_applied(&self, id: &str) -> bool {
        matches!(
            self.processed.get(id).map(|r| r.state),
            Some(ProcessedState::Applied)
        )
    }

    pub fn metrics(&self) -> OfflineSyncQueueMetrics {
        OfflineSyncQueueMetrics {
            received: self.received.load(Ordering::Relaxed),
            applied: self.applied.load(Ordering::Relaxed),
            duplicates: self.duplicates.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn make_action(id: &str) -> OfflineAction {
        OfflineAction {
            id: id.to_string(),
            method: OfflineActionMethod::Post,
            resource: "corridor:us-mx".to_string(),
            payload: serde_json::json!({ "rate": 1.2 }),
            client_timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn first_submission_applies() {
        let queue = OfflineSyncQueue::new();
        let outcome = queue
            .submit(make_action("a1"), |_| async { Ok(()) })
            .await
            .unwrap();

        assert_eq!(outcome, OfflineActionOutcome::Applied);
        assert!(queue.has_applied("a1"));
        assert_eq!(queue.metrics().applied, 1);
    }

    #[tokio::test]
    async fn duplicate_id_is_not_reapplied() {
        let queue = OfflineSyncQueue::new();
        let calls = Arc::new(AtomicUsize::new(0));

        for _ in 0..3 {
            let calls = Arc::clone(&calls);
            queue
                .submit(make_action("a1"), move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async { Ok(()) }
                })
                .await
                .unwrap();
        }

        assert_eq!(calls.load(Ordering::SeqCst), 1, "apply must run exactly once");
        let metrics = queue.metrics();
        assert_eq!(metrics.received, 3);
        assert_eq!(metrics.applied, 1);
        assert_eq!(metrics.duplicates, 2);
    }

    #[tokio::test]
    async fn concurrent_submissions_of_the_same_id_apply_exactly_once() {
        let queue = Arc::new(OfflineSyncQueue::new());
        let calls = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let queue = Arc::clone(&queue);
            let calls = Arc::clone(&calls);
            handles.push(tokio::spawn(async move {
                queue
                    .submit(make_action("racey"), move |_| {
                        let calls = Arc::clone(&calls);
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                            Ok(())
                        }
                    })
                    .await
                    .unwrap()
            }));
        }

        let outcomes: Vec<OfflineActionOutcome> =
            futures::future::join_all(handles).await.into_iter().map(|r| r.unwrap()).collect();

        assert_eq!(calls.load(Ordering::SeqCst), 1, "apply must run exactly once under concurrency");
        assert_eq!(
            outcomes.iter().filter(|o| **o == OfflineActionOutcome::Applied).count(),
            1
        );
        assert_eq!(
            outcomes.iter().filter(|o| **o == OfflineActionOutcome::Duplicate).count(),
            7
        );
    }

    #[tokio::test]
    async fn failed_apply_releases_the_id_for_retry() {
        let queue = OfflineSyncQueue::new();

        let first = queue
            .submit(make_action("a1"), |_| async {
                Err(anyhow::anyhow!("downstream unavailable"))
            })
            .await;
        assert!(first.is_err());
        assert!(!queue.has_applied("a1"));
        assert_eq!(queue.metrics().failed, 1);

        let second = queue
            .submit(make_action("a1"), |_| async { Ok(()) })
            .await
            .unwrap();

        assert_eq!(second, OfflineActionOutcome::Applied);
        assert!(queue.has_applied("a1"));
    }

    #[tokio::test]
    async fn different_ids_are_independent() {
        let queue = OfflineSyncQueue::new();

        queue.submit(make_action("a1"), |_| async { Ok(()) }).await.unwrap();
        queue.submit(make_action("a2"), |_| async { Ok(()) }).await.unwrap();

        assert!(queue.has_applied("a1"));
        assert!(queue.has_applied("a2"));
        assert_eq!(queue.metrics().applied, 2);
        assert_eq!(queue.metrics().duplicates, 0);
    }
}
