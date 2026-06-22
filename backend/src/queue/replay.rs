//! Idempotent replay processor for messages produced by the mobile
//! `sync_queue` table (issue #93).
//!
//! [`QueueProcessor`] accepts a stream of [`QueueMessage`]s and ensures
//! that, regardless of how many times the same `dedup_key` is delivered,
//! the side-effects only happen once. It tracks processed dedup keys in
//! an in-process LRU when no persistent store is wired in; production
//! deployments are expected to back this with the metrics database (see
//! `db::aggregation`).

use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::types::{QueueMessage, QueueMessageStatus, QueueMessageType};

const DEFAULT_MAX_RETRIES: u32 = 5;

#[derive(Debug, Error)]
pub enum QueueReplayError {
    #[error("queue message could not be processed: {message}")]
    Processing { message: String },
    #[error("invalid queue message: {0}")]
    Invalid(String),
}

/// Strategy that performs the actual side-effects. The default
/// `LoggingProcessor` is used by tests; production swaps in a real
/// implementation backed by the database.
#[async_trait::async_trait]
pub trait QueueProcessorHandler: Send + Sync {
    async fn handle(&self, dedup_key: &str, message: &QueueMessageType) -> Result<(), String>;
}

/// Fallback handler that just logs. Reduces test surface and keeps the
/// processor usable without a database dependency.
pub struct LoggingProcessor;

#[async_trait::async_trait]
impl QueueProcessorHandler for LoggingProcessor {
    async fn handle(
        &self,
        dedup_key: &str,
        message: &QueueMessageType,
    ) -> Result<(), String> {
        info!(dedup_key, message = ?message, "queue: processed offline replay");
        Ok(())
    }
}

/// Idempotent queue processor.
///
/// Holds a mutex-protected set of `dedup_key`s it has already processed
/// within this process lifetime, plus a counter of attempts per dedup key
/// so retries cannot loop indefinitely.
pub struct QueueProcessor {
    processed: Arc<Mutex<HashSet<String>>>,
    attempts: Arc<Mutex<std::collections::HashMap<String, u32>>>,
    handler: Arc<dyn QueueProcessorHandler>,
    max_retries: u32,
}

impl QueueProcessor {
    #[must_use]
    pub fn new(handler: Arc<dyn QueueProcessorHandler>) -> Self {
        Self::with_max_retries(handler, DEFAULT_MAX_RETRIES)
    }

    #[must_use]
    pub fn with_max_retries(
        handler: Arc<dyn QueueProcessorHandler>,
        max_retries: u32,
    ) -> Self {
        Self {
            processed: Arc::new(Mutex::new(HashSet::new())),
            attempts: Arc::new(Mutex::new(std::collections::HashMap::new())),
            handler,
            max_retries,
        }
    }

    /// Process a single queue message idempotently.
    ///
    /// * If the `dedup_key` has already been processed in this process,
    ///   the call short-circuits with `Ok(())` (no side-effects, no error
    ///   counter increment).
    /// * If processing fails, the per-key attempts counter is bumped.
    ///   The message is reported as failed once `max_retries` is reached.
    pub async fn process(
        &self,
        message: QueueMessage,
    ) -> Result<QueueMessageStatus, QueueReplayError> {
        if message.dedup_key.is_empty() {
            return Err(QueueReplayError::Invalid(
                "dedup_key cannot be empty".to_string(),
            ));
        }

        {
            let processed = self.processed.lock().await;
            if processed.contains(&message.dedup_key) {
                debug!(
                    dedup_key = %message.dedup_key,
                    "queue: skipping already-processed dedup_key"
                );
                return Ok(QueueMessageStatus::Processed);
            }
        }

        match self.handler.handle(&message.dedup_key, &message.message).await {
            Ok(()) => {
                let mut processed = self.processed.lock().await;
                processed.insert(message.dedup_key.clone());
                Ok(QueueMessageStatus::Processed)
            }
            Err(err) => {
                let mut attempts = self.attempts.lock().await;
                let count = attempts
                    .entry(message.dedup_key.clone())
                    .or_insert(0);
                *count += 1;
                let n = *count;
                if n >= self.max_retries {
                    warn!(
                        dedup_key = %message.dedup_key,
                        attempts = n,
                        error = %err,
                        "queue: giving up after exhausting retries"
                    );
                    return Ok(QueueMessageStatus::Failed);
                }
                warn!(
                    dedup_key = %message.dedup_key,
                    attempts = n,
                    error = %err,
                    "queue: handler failed, will be retried"
                );
                Err(QueueReplayError::Processing { message: err })
            }
        }
    }

    /// Synchronously replay a batch of messages, returning one status per
    /// input in the same order. Useful for backfills or for processing
    /// the mobile sync_queue snapshot on reconnect.
    pub async fn replay_batch(
        &self,
        messages: Vec<QueueMessage>,
    ) -> Vec<Result<QueueMessageStatus, QueueReplayError>> {
        let mut out = Vec::with_capacity(messages.len());
        for msg in messages {
            out.push(self.process(msg).await);
        }
        out
    }

    /// Convenience helper for callers that have raw (id, dedup, type)
    /// tuples rather than full `QueueMessage`s — preserves backwards
    /// compatibility with callers migrating from the legacy
    /// `message_queue_system` module.
    pub fn build_message(
        dedup_key: &str,
        message: QueueMessageType,
    ) -> Result<QueueMessage, QueueReplayError> {
        if dedup_key.is_empty() {
            return Err(QueueReplayError::Invalid(
                "dedup_key cannot be empty".to_string(),
            ));
        }
        let now = Utc::now();
        Ok(QueueMessage {
            id: Uuid::new_v4(),
            dedup_key: dedup_key.to_string(),
            message,
            status: QueueMessageStatus::Pending,
            created_at: now,
            updated_at: now,
            attempts: 0,
            last_error: None,
        })
    }

    /// Snapshot of processed dedup keys for diagnostic tooling (used by
    /// `backend/src/debugging` when surfacing queue health).
    pub async fn processed_dedup_keys(&self) -> Vec<String> {
        self.processed
            .lock()
            .await
            .iter()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct CountingProcessor {
        calls: Arc<AtomicU32>,
    }

    #[async_trait::async_trait]
    impl QueueProcessorHandler for CountingProcessor {
        async fn handle(
            &self,
            _dedup_key: &str,
            _message: &QueueMessageType,
        ) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    struct FlakyProcessor {
        fail_until_attempt: Arc<Mutex<u32>>,
    }

    #[async_trait::async_trait]
    impl QueueProcessorHandler for FlakyProcessor {
        async fn handle(
            &self,
            _dedup_key: &str,
            _message: &QueueMessageType,
        ) -> Result<(), String> {
            let mut guard = self.fail_until_attempt.lock().await;
            *guard += 1;
            if *guard <= 2 {
                Err(format!("simulated failure {}", *guard))
            } else {
                Ok(())
            }
        }
    }

    fn build_message(dedup_key: &str) -> QueueMessage {
        QueueProcessor::build_message(
            dedup_key,
            QueueMessageType::RecordUsageEvent {
                event_name: "test".to_string(),
                properties: serde_json::json!({}),
            },
        )
        .expect("message")
    }

    #[tokio::test]
    async fn replay_is_idempotent() {
        let calls = Arc::new(AtomicU32::new(0));
        let processor = QueueProcessor::new(Arc::new(CountingProcessor {
            calls: calls.clone(),
        }));

        let msg = build_message("dedup-1");
        let r1 = processor.process(msg.clone()).await.unwrap();
        let r2 = processor.process(msg.clone()).await.unwrap();
        let r3 = processor.process(msg).await.unwrap();

        assert_eq!(r1, QueueMessageStatus::Processed);
        assert_eq!(r2, QueueMessageStatus::Processed);
        assert_eq!(r3, QueueMessageStatus::Processed);
        // Handler must fire exactly once even though the message was
        // submitted three times.
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn replay_invalid_dedup_key_rejected() {
        let processor =
            QueueProcessor::new(Arc::new(LoggingProcessor));
        // Build message directly (bypass build_message) so the empty
        // dedup_key reaches QueueProcessor::process.
        let msg = QueueMessage {
            id: Uuid::new_v4(),
            dedup_key: String::new(),
            message: QueueMessageType::RecordUsageEvent {
                event_name: "x".into(),
                properties: serde_json::json!({}),
            },
            status: QueueMessageStatus::Pending,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            attempts: 0,
            last_error: None,
        };
        let result = processor.process(msg).await;
        assert!(matches!(result, Err(QueueReplayError::Invalid(_))));
    }

    #[tokio::test]
    async fn retry_then_succeed() {
        let flaky = Arc::new(FlakyProcessor {
            fail_until_attempt: Arc::new(Mutex::new(0)),
        });
        let processor = QueueProcessor::new(flaky.clone());
        let msg = build_message("dedup-flaky");

        let r1 = processor.process(msg.clone()).await;
        let r2 = processor.process(msg.clone()).await;
        let r3 = processor.process(msg).await;

        assert!(r1.is_err());
        assert!(r2.is_err());
        assert_eq!(r3.unwrap(), QueueMessageStatus::Processed);
    }

    #[tokio::test]
    async fn replay_batch_preserves_order() {
        let processor = QueueProcessor::new(Arc::new(LoggingProcessor));
        let messages = vec![
            build_message("dedup-a"),
            build_message("dedup-b"),
            build_message("dedup-c"),
        ];
        let results = processor.replay_batch(messages).await;
        assert_eq!(results.len(), 3);
        for r in results {
            assert_eq!(r.unwrap(), QueueMessageStatus::Processed);
        }
    }

    #[tokio::test]
    async fn give_up_after_max_retries() {
        // Always-fail handler
        struct AlwaysFail;
        #[async_trait::async_trait]
        impl QueueProcessorHandler for AlwaysFail {
            async fn handle(
                &self,
                _k: &str,
                _m: &QueueMessageType,
            ) -> Result<(), String> {
                Err("boom".to_string())
            }
        }

        let processor =
            QueueProcessor::with_max_retries(Arc::new(AlwaysFail), 3);
        let msg = build_message("dedup-fail");

        for _ in 0..2 {
            let _ = processor.process(msg.clone()).await;
        }
        let final_status = processor.process(msg).await.unwrap();
        assert_eq!(final_status, QueueMessageStatus::Failed);
    }
}
