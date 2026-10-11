//! The primitive operations a storage engine provides. The `Store` builds the
//! game-level logic (genesis, snapshots, replay, rebasing, backups) on top of
//! these, once, so every backend shares the same guarantees and tests.
//!
//! Values are the store's own JSON encodings; backends never interpret them.

use crate::Result;

/// Raw tables, for tests that tamper with data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Table {
    Journal,
    Snapshots,
}

/// One row of the interaction log: telemetry about who called what, kept beside the
/// universe but outside the journal, so it never affects replay or the world hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityRow {
    pub at_ms: u64,
    pub player: Option<u64>,
    pub data: Vec<u8>,
}

/// One row of the permanent daily activity summary (`activity_daily`): one captain's (or, with
/// player 0, everyone not logged in) calls on one UTC day. Unlike the raw log it is kept forever
/// and copied by export and import. `data` is the summary's JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DailyRow {
    /// The UTC day, "YYYY-MM-DD".
    pub day: String,
    pub player: u64,
    pub data: Vec<u8>,
}

pub trait Backend: Send {
    /// Human-readable location (a path, or a URL without its password).
    fn describe(&self) -> String;
    fn meta_get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    /// Write several meta keys in one transaction.
    fn meta_put(&self, pairs: &[(&str, &[u8])]) -> Result<()>;
    /// Durably append entry `seq` (which must be last + 1) and, if given, its
    /// checkpoint hash, in one transaction.
    fn append(&self, seq: u64, entry: &[u8], checkpoint: Option<&str>, private: &[crate::private_archive::ArchiveRow]) -> Result<()>;
    /// Append-only permanent private history. Backfill is idempotent.
    fn private_insert(&self, rows: &[crate::private_archive::ArchiveRow]) -> Result<()>;
    fn private_query(&self, q: &crate::private_archive::ArchiveQuery) -> Result<Vec<crate::private_archive::ArchiveRow>>;
    fn last_seq(&self) -> Result<u64>;
    fn journal_len(&self) -> Result<u64>;
    /// Up to `limit` journal rows with seq > `after`, in order.
    fn entries_page(&self, after: u64, limit: usize) -> Result<Vec<(u64, Vec<u8>)>>;
    /// Newest journal rows first, strictly before the optional sequence cursor.
    fn entries_before(&self, before: Option<u64>, limit: usize) -> Result<Vec<(u64, Vec<u8>)>>;
    fn checkpoints_after(&self, after: u64) -> Result<Vec<(u64, String)>>;
    /// Store a snapshot row and keep only the newest `keep`.
    fn snapshot_put(&self, seq: u64, row: &[u8], keep: usize) -> Result<()>;
    fn snapshot_last(&self) -> Result<Option<(u64, Vec<u8>)>>;
    fn snapshot_last_seq(&self) -> Result<Option<u64>>;
    /// Bulk-insert journal rows and checkpoints in one transaction (copying a
    /// universe between backends; the caller guarantees order and continuity).
    fn bulk_load(&self, entries: &[(u64, Vec<u8>)], checkpoints: &[(u64, String)]) -> Result<()>;
    /// Append interaction-log rows (writer only).
    fn activity_append(&self, rows: &[ActivityRow]) -> Result<()>;
    /// Interaction-log rows at or after `since_ms`, newest first, for one player or all, at most `limit`.
    fn activity_query(&self, player: Option<u64>, since_ms: u64, limit: usize) -> Result<Vec<ActivityRow>>;
    /// Delete interaction-log rows older than `before_ms`; returns how many.
    fn activity_prune(&self, before_ms: u64) -> Result<u64>;
    /// Interaction-log rows with `from_ms <= at_ms < to_ms`, oldest first.
    fn activity_between(&self, from_ms: u64, to_ms: u64) -> Result<Vec<ActivityRow>>;
    /// The time of the oldest interaction-log row, if any.
    fn activity_oldest(&self) -> Result<Option<u64>>;
    /// Replace every daily summary row of `day` with `rows` (all of that day), in one transaction.
    fn daily_replace(&self, day: &str, rows: &[DailyRow]) -> Result<()>;
    /// Daily summary rows with `from <= day <= to` (inclusive, "YYYY-MM-DD"), for one player or
    /// all, ordered by day then player, at most `limit`.
    fn daily_query(&self, from: &str, to: &str, player: Option<u64>, limit: usize) -> Result<Vec<DailyRow>>;
    /// Every day that has daily summary rows, oldest first.
    fn daily_days(&self) -> Result<Vec<String>>;
    /// End a read session (a consistent export reads inside one transaction).
    fn finish(&self) -> Result<()> {
        Ok(())
    }
    /// Overwrite a raw row (tests only: simulating tampering and schema drift).
    #[doc(hidden)]
    fn debug_put(&self, table: Table, key: u64, value: &[u8]) -> Result<()>;
    #[doc(hidden)]
    fn debug_get(&self, table: Table, key: u64) -> Result<Option<Vec<u8>>>;
}
