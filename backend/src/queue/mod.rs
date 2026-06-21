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
