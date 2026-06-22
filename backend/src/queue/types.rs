//! Queue message types shared between the backend queue processor and the
//! mobile `sync_queue` table. Every variant carries the mobile-issued
//! `dedup_key` so the backend can de-duplicate retries.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Domain actions that can be enqueued from the mobile client. Persisted
/// actions handled offline and replayed when connectivity returns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QueueMessageType {
    /// User toggled a favourite anchor – no server mutation beyond
    /// upserting the user's preference row. Idempotent on `dedup_key`.
    SetAnchorFavorite {
        anchor_id: Uuid,
        favorite: bool,
    },
    /// Mobile recorded a usage event that is replayed asynchronously.
    RecordUsageEvent {
        event_name: String,
        properties: serde_json::Value,
    },
    /// Submit a (validated) payment observation so the analytics pipeline
    /// can include it in aggregated metrics.
    SubmitPaymentObservation {
        payment_id: String,
        corridor_key: String,
        observed_at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueueMessageStatus {
    Pending,
    Processed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueMessage {
    pub id: Uuid,
    /// Mobile-issued dedup key. Re-enqueueing the same logical operation
    /// must reuse this key so the backend can short-circuit idempotently.
    pub dedup_key: String,
    pub message: QueueMessageType,
    pub status: QueueMessageStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub attempts: u32,
    #[serde(default)]
    pub last_error: Option<String>,
}
