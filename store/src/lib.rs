//! Durable storage for the deterministic engine, on an embedded redb file
//! (local play, development, portable backups) or Postgres (hosted production).
//!
//! Tables (same in both):
//! * `meta`: `schema_version`, genesis parameters, and the replay *base*: the
//!   world (and engine version) that journal replay starts from.
//! * `journal`: every accepted command, keyed by sequence number.
//! * `checkpoints`: world hash after every `CHECKPOINT_EVERY`-th entry, written
//!   in the same transaction as that entry.
//! * `snapshots`: periodic `{hash, world}` snapshots (latest few kept).
//! * `private_archive`: permanent full private versions and archive events, outside World,
//!   committed with the journal and copied by export/import. Never pruned.
//! * `activity`: the raw interaction log (telemetry, pruned after a retention period, not
//!   exported).
//! * `activity_daily`: one summary row per UTC day and captain, kept forever and copied by
//!   export and import. Both activity tables are created on open in every backend, so they need
//!   no schema migration.
//!
//! Boot = latest snapshot (hash-checked) + replay of later entries, comparing
//! checkpoint hashes on the way. Each append commits durably before the player
//! gets a reply.
//!
//! Compatibility rules:
//! * Stores without a schema version, or from a newer build, are refused before
//!   any world is deserialized.
//! * A universe recorded by a different engine version is refused by default.
//!   `adopt_universe` rebases it onto the current engine from a snapshot taken
//!   at the head of the journal (old history is kept but no longer replayed).

pub mod backend;
pub mod pg;
pub mod private_archive;
mod redb_backend;

use std::path::{Path, PathBuf};

use af_core::engine::{Entry, apply};
use af_core::galaxy::{GenesisConfig, generate};
use af_core::rules::Rules;
use af_core::{ENGINE_VERSION, World};
use serde::{Deserialize, Serialize};

pub use backend::{ActivityRow, Backend, DailyRow, Table};
pub use pg::Access;
use pg::PgBackend;
use redb_backend::RedbBackend;

const KEEP_SNAPSHOTS: usize = 3;
/// Journal rows fetched per query when replaying or copying.
const PAGE: usize = 5_000;
/// Meta keys that make up a universe (besides `schema_version`).
const UNIVERSE_KEYS: [&str; 3] = ["genesis", "base", "base_world"];

/// Write a world-hash checkpoint after every N-th journal entry.
pub const CHECKPOINT_EVERY: u64 = 100;

/// Storage layout version. Bump together with a new entry in `MIGRATIONS`.
pub const SCHEMA_VERSION: u64 = 1;

type Migration = fn(&dyn Backend) -> Result<()>;
/// `MIGRATIONS[i]` upgrades a store from schema version `i + 1` to `i + 2`.
const MIGRATIONS: &[Migration] = &[];
const _: () = assert!(SCHEMA_VERSION == 1 + MIGRATIONS.len() as u64, "one migration per schema bump");

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Db(String),
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("store has no genesis record; initialise it first")]
    NoGenesis,
    #[error(
        "this database has no schema version: it was written by a pre-release build and cannot be opened. Archive it and start a new universe"
    )]
    Unversioned,
    #[error("this database uses schema version {found}, newer than this build supports ({supported}); upgrade af-server")]
    NewerFile { found: u64, supported: u64 },
    #[error(
        "this universe was recorded by engine '{file}' but this binary is '{binary}'. Start the server with --adopt-universe to rebase it onto the new rules (needs a snapshot at the head of the journal: stop the old server gracefully first), or run the matching build"
    )]
    EngineMismatch { file: String, binary: String },
    #[error(
        "cannot adopt: the latest snapshot (seq {snapshot}) is not at the head of the journal (seq {head}); stop the old server gracefully so it writes one"
    )]
    NotAtHead { snapshot: u64, head: u64 },
    #[error("snapshot at seq {seq} is corrupt: stored hash {stored} but contents hash to {actual}")]
    Corrupt { seq: u64, stored: String, actual: String },
    #[error("replay diverged at seq {seq}: checkpoint hash {expected}, replay produced {got}")]
    Diverged { seq: u64, expected: String, got: String },
    #[error("journal entry {seq} was refused on replay ({reason}); the engine and journal disagree")]
    Replay { seq: u64, reason: String },
    #[error("journal append out of order: expected seq {expected}, got {got}")]
    OutOfOrder { expected: u64, got: u64 },
    #[error("another server is the writer for the universe at {0}")]
    Busy(String),
    #[error("the destination already holds a universe; copy into an empty database")]
    NotEmpty,
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Genesis {
    pub seed: u64,
    pub sectors: u32,
    pub rules: Rules,
    pub created_unix_ms: u64,
    /// Engine code version that created this universe.
    #[serde(default)]
    pub engine: String,
}

impl Genesis {
    pub fn config(&self) -> GenesisConfig {
        GenesisConfig {
            seed: self.seed,
            sectors: self.sectors,
            rules: self.rules.clone(),
        }
    }
}

/// Where replay starts: the world at `seq`, recorded by engine `engine`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Base {
    pub seq: u64,
    pub engine: String,
    /// Earlier bases this universe was rebased from (oldest first).
    #[serde(default)]
    pub history: Vec<(u64, String)>,
}

#[derive(Serialize, Deserialize)]
struct SnapshotRow {
    hash: String,
    world: World,
}

/// Where a store lives.
#[derive(Clone, Debug)]
enum Location {
    File(PathBuf),
    Postgres(String),
}

/// A one-off job that writes a consistent portable copy of the universe to a
/// redb file and returns its size. It runs on its own thread while the writer
/// keeps going.
pub type BackupJob = Box<dyn FnOnce(&Path) -> Result<u64> + Send>;

pub struct Store {
    b: Box<dyn Backend>,
    loc: Location,
}

impl Store {
    /// Open or create an embedded database file (the single writer: redb locks the file).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        Self::check_schema(Box::new(RedbBackend::open(&path)?), Location::File(path), true)
    }

    /// Open a store by location: a `postgres://` URL or a file path. File
    /// stores are always opened as the writer.
    pub fn open_location(loc: &str, access: Access) -> Result<Self> {
        if pg::is_postgres(loc) {
            let b = PgBackend::open(loc, access)?;
            Self::check_schema(Box::new(b), Location::Postgres(loc.to_string()), access == Access::Writer)
        } else {
            Self::open(loc)
        }
    }

    /// Validate and migrate the schema version (writers only write).
    fn check_schema(b: Box<dyn Backend>, loc: Location, writer: bool) -> Result<Self> {
        let version: Option<u64> = match b.meta_get("schema_version")? {
            Some(v) => Some(serde_json::from_slice(&v)?),
            None => None,
        };
        let has_genesis = b.meta_get("genesis")?.is_some();
        let stamp = |b: &dyn Backend| b.meta_put(&[("schema_version", &serde_json::to_vec(&SCHEMA_VERSION)?)]);
        match version {
            None if has_genesis => return Err(StoreError::Unversioned),
            None if writer => stamp(b.as_ref())?,
            None => {}
            Some(v) if v > SCHEMA_VERSION => {
                return Err(StoreError::NewerFile {
                    found: v,
                    supported: SCHEMA_VERSION,
                });
            }
            Some(v) if v < SCHEMA_VERSION => {
                if !writer {
                    return Err(StoreError::Db(format!(
                        "schema version {v} needs migrating; open it as the writer first"
                    )));
                }
                for m in &MIGRATIONS[(v - 1) as usize..] {
                    m(b.as_ref())?;
                }
                stamp(b.as_ref())?;
            }
            Some(_) => {}
        }
        Ok(Store { b, loc })
    }

    /// Where this store lives (a path, or a URL without its password).
    pub fn describe(&self) -> String {
        self.b.describe()
    }

    pub fn is_postgres(&self) -> bool {
        matches!(self.loc, Location::Postgres(_))
    }

    /// Raw backend access (tests and tools).
    #[doc(hidden)]
    pub fn backend(&self) -> &dyn Backend {
        self.b.as_ref()
    }

    /// Write a consistent, portable copy of the universe to a redb file at `dest`.
    /// For a file store, call it only from the single writer between transactions
    /// (the engine actor does): every commit is durable, so the file copy is a
    /// consistent universe. A Postgres store reads in one repeatable-read
    /// transaction, so it is consistent whenever it runs.
    pub fn backup_to(&self, dest: &Path) -> Result<u64> {
        match &self.loc {
            Location::File(path) => {
                let tmp = dest.with_extension("partial");
                let io = |e: std::io::Error| StoreError::Db(format!("backup: {e}"));
                let n = std::fs::copy(path, &tmp).map_err(io)?;
                std::fs::rename(&tmp, dest).map_err(io)?;
                Ok(n)
            }
            Location::Postgres(url) => export_pg(url, dest),
        }
    }

    /// A backup that can run off the writer's thread, if this store supports it.
    pub fn detached_backup(&self) -> Option<BackupJob> {
        match &self.loc {
            Location::File(_) => None,
            Location::Postgres(url) => {
                let url = url.clone();
                Some(Box::new(move |dest: &Path| export_pg(&url, dest)))
            }
        }
    }

    /// Copy the whole universe (journal, checkpoints, latest snapshot, base) into
    /// `dest`, which must be empty. Used for backups, restores and migrations.
    pub fn copy_into(&self, dest: &Store) -> Result<u64> {
        if dest.b.meta_get("genesis")?.is_some() || dest.b.last_seq()? > 0 {
            return Err(StoreError::NotEmpty);
        }
        if self.b.meta_get("genesis")?.is_none() {
            return Err(StoreError::NoGenesis);
        }
        let checkpoints = self.b.checkpoints_after(0)?;
        let mut cp = checkpoints.as_slice();
        let mut after = 0;
        let mut copied = 0u64;
        loop {
            let page = self.b.entries_page(after, PAGE)?;
            let Some(last) = page.last().map(|(s, _)| *s) else { break };
            let split = cp.partition_point(|(s, _)| *s <= last);
            dest.b.bulk_load(&page, &cp[..split])?;
            cp = &cp[split..];
            copied += page.len() as u64;
            after = last;
        }
        if !cp.is_empty() {
            dest.b.bulk_load(&[], cp)?;
        }
        if let Some((seq, row)) = self.b.snapshot_last()? {
            dest.b.snapshot_put(seq, &row, KEEP_SNAPSHOTS)?;
        }
        // The permanent daily activity summaries travel with the universe (the raw log does not).
        for day in self.b.daily_days()? {
            let rows = self.b.daily_query(&day, &day, None, usize::MAX)?;
            dest.b.daily_replace(&day, &rows)?;
        }
        let mut cursor = private_archive::ArchiveQuery {
            limit: PAGE,
            ..Default::default()
        };
        loop {
            let rows = self.b.private_query(&cursor)?;
            let Some(last) = rows.last() else { break };
            cursor.after_seq = last.seq;
            cursor.after_ordinal = last.ordinal;
            dest.b.private_insert(&rows)?;
        }
        if let Some(marker) = self.b.meta_get("private_archive_backfilled")? {
            dest.b.meta_put(&[("private_archive_backfilled", &marker)])?;
        }
        // The universe keys go last: a copy cut short has no genesis and is not a universe.
        let mut meta = vec![];
        for k in UNIVERSE_KEYS {
            meta.push((k, self.b.meta_get(k)?.ok_or(StoreError::NoGenesis)?));
        }
        let pairs: Vec<(&str, &[u8])> = meta.iter().map(|(k, v)| (*k, v.as_slice())).collect();
        dest.b.meta_put(&pairs)?;
        self.b.finish()?;
        Ok(copied)
    }

    pub fn genesis(&self) -> Result<Option<Genesis>> {
        match self.b.meta_get("genesis")? {
            Some(v) => Ok(Some(serde_json::from_slice(&v)?)),
            None => Ok(None),
        }
    }

    pub fn base(&self) -> Result<Base> {
        match self.b.meta_get("base")? {
            Some(v) => Ok(serde_json::from_slice(&v)?),
            None => Err(StoreError::NoGenesis),
        }
    }

    /// Engine version the replay base was recorded with (reads metadata only).
    pub fn check_engine(&self) -> Result<()> {
        let b = self.base()?;
        if b.engine != ENGINE_VERSION {
            return Err(StoreError::EngineMismatch {
                file: b.engine,
                binary: ENGINE_VERSION.into(),
            });
        }
        Ok(())
    }

    /// Record genesis parameters *and* the generated world, so replay never
    /// depends on re-running (float-based) galaxy generation.
    pub fn init(&self, g: &Genesis) -> Result<World> {
        let world = generate(&g.config());
        let base = Base {
            seq: 0,
            engine: g.engine.clone(),
            history: vec![],
        };
        self.b.meta_put(&[
            ("genesis", &serde_json::to_vec(g)?),
            ("base", &serde_json::to_vec(&base)?),
            ("base_world", &serde_json::to_vec(&world)?),
        ])?;
        Ok(world)
    }

    fn base_world(&self) -> Result<World> {
        let v = self.b.meta_get("base_world")?.ok_or(StoreError::NoGenesis)?;
        Ok(serde_json::from_slice(&v)?)
    }

    /// Durably append one journal entry, plus its world-hash checkpoint when given.
    pub fn append(&self, e: &Entry, checkpoint: Option<&str>) -> Result<()> {
        self.b.append(e.seq, &serde_json::to_vec(e)?, checkpoint, &[])
    }

    /// The archive and journal commit together before any successful reply.
    pub fn append_with_private(&self, e: &Entry, checkpoint: Option<&str>, rows: &[private_archive::ArchiveRow]) -> Result<()> {
        self.b.append(e.seq, &serde_json::to_vec(e)?, checkpoint, rows)
    }

    pub fn private_archive(&self, q: &private_archive::ArchiveQuery) -> Result<Vec<private_archive::ArchiveRow>> {
        self.b.private_query(q)
    }

    /// One-time historical index. Journal rows are never altered or discarded.
    /// An interrupted backfill retries safely because archive row identities are immutable.
    pub fn backfill_private(&self, w: &World) -> Result<()> {
        if self.b.meta_get("private_archive_backfilled")?.is_some() {
            return Ok(());
        }
        let mut projection = private_archive::Capture::default();
        let mut after = 0;
        loop {
            let page = self.b.entries_page(after, PAGE)?;
            if page.is_empty() {
                break;
            }
            let mut rows = vec![];
            for (seq, data) in page {
                after = seq;
                // Unrelated historical commands can have obsolete schemas. Parse only private
                // commands, resets and ticks, preserving all original journal bytes.
                let v: serde_json::Value = serde_json::from_slice(&data)?;
                let typ = v["cmd"]["type"].as_str().unwrap_or("");
                let action = v["cmd"]["args"]["action"].as_str().unwrap_or("");
                if matches!(typ, "note" | "memory" | "tick")
                    || (typ == "admin" && matches!(action, "fresh_start" | "clear_notes" | "remove_player"))
                {
                    let e: Entry = serde_json::from_value(v)?;
                    rows.extend(projection.legacy_step(&e)?);
                }
            }
            self.b.private_insert(&rows)?;
        }
        // Seed complete extant records too, including private state predating an imported base.
        let mut rows = vec![];
        for p in w.players.values().filter(|p| p.npc.is_none()) {
            for (kind, k, body) in p.notes.iter().map(|(k, v)| ("note", k, serde_json::json!({"text":v}))).chain(
                p.memory
                    .iter()
                    .map(|(k, r)| ("memory", k, serde_json::json!({"versions":r.versions}))),
            ) {
                let ordinal = 1_000_000 + rows.len() as u64;
                let data = serde_json::json!({"seq":w.seq,"ordinal":ordinal,"player":p.id,"name":p.name,"kind":kind,"key":k,"tick":w.tick,"epoch":w.archive.epoch,"season":w.season.number,"reason":"upgrade_snapshot","body":body});
                rows.push(private_archive::ArchiveRow {
                    seq: w.seq,
                    ordinal,
                    player: u64::from(p.id),
                    kind: kind.into(),
                    key: k.clone(),
                    data: serde_json::to_vec(&data)?,
                });
            }
        }
        self.b.private_insert(&rows)?;
        self.b.meta_put(&[("private_archive_backfilled", b"true")])?;
        Ok(())
    }

    /// Append interaction-log rows (telemetry; never part of the journal or replay).
    /// Operator console settings (alert acknowledgements), kept beside the universe under
    /// `console:<key>` in the meta table: never journaled, never replayed, not exported.
    pub fn console_get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.b.meta_get(&format!("console:{key}"))
    }

    pub fn console_put(&self, key: &str, value: &[u8]) -> Result<()> {
        self.b.meta_put(&[(&format!("console:{key}"), value)])
    }

    pub fn activity_append(&self, rows: &[ActivityRow]) -> Result<()> {
        self.b.activity_append(rows)
    }

    /// Interaction-log rows since `since_ms`, newest first.
    pub fn activity(&self, player: Option<u64>, since_ms: u64, limit: usize) -> Result<Vec<ActivityRow>> {
        self.b.activity_query(player, since_ms, limit)
    }

    /// Drop interaction-log rows older than `before_ms`.
    pub fn activity_prune(&self, before_ms: u64) -> Result<u64> {
        self.b.activity_prune(before_ms)
    }

    /// Interaction-log rows with `from_ms <= at_ms < to_ms`, oldest first.
    pub fn activity_between(&self, from_ms: u64, to_ms: u64) -> Result<Vec<ActivityRow>> {
        self.b.activity_between(from_ms, to_ms)
    }

    /// The time of the oldest interaction-log row.
    pub fn activity_oldest(&self) -> Result<Option<u64>> {
        self.b.activity_oldest()
    }

    /// Replace one UTC day's daily activity summaries.
    pub fn daily_replace(&self, day: &str, rows: &[DailyRow]) -> Result<()> {
        self.b.daily_replace(day, rows)
    }

    /// Daily activity summaries from `from` to `to` (inclusive days), by day then player.
    pub fn daily_query(&self, from: &str, to: &str, player: Option<u64>, limit: usize) -> Result<Vec<DailyRow>> {
        self.b.daily_query(from, to, player, limit)
    }

    /// Every day with daily activity summaries, oldest first.
    pub fn daily_days(&self) -> Result<Vec<String>> {
        self.b.daily_days()
    }

    /// On Postgres, the interaction log on a connection of its own (see `pg::ActivityLog`);
    /// None for a redb file, where the engine thread keeps writing it.
    pub fn activity_log(&self) -> Option<pg::ActivityLog> {
        match &self.loc {
            Location::Postgres(url) => Some(pg::ActivityLog::new(url)),
            Location::File(_) => None,
        }
    }

    pub fn journal_len(&self) -> Result<u64> {
        self.b.journal_len()
    }

    pub fn last_seq(&self) -> Result<u64> {
        self.b.last_seq()
    }

    /// Entries with seq > `after`, in order.
    pub fn entries_after(&self, after: u64) -> Result<Vec<Entry>> {
        let mut out = vec![];
        let mut after = after;
        loop {
            let page = self.b.entries_page(after, PAGE)?;
            let Some((last, _)) = page.last() else { break };
            after = *last;
            for (_, v) in &page {
                out.push(serde_json::from_slice(v)?);
            }
            if page.len() < PAGE {
                break;
            }
        }
        Ok(out)
    }

    /// A bounded reverse page, parsed as JSON so pre-upgrade command shapes stay searchable.
    pub fn journal_page(&self, before: Option<u64>, limit: usize) -> Result<Vec<serde_json::Value>> {
        self.b
            .entries_before(before, limit.min(1_001))?
            .into_iter()
            .map(|(_, bytes)| serde_json::from_slice(&bytes).map_err(StoreError::from))
            .collect()
    }

    /// Every journal entry as stored, unparsed: for offline reports that must read entries
    /// recorded by older engines (`af-server conduct`).
    pub fn raw_entries(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let mut out = vec![];
        let mut after = 0;
        loop {
            let page = self.b.entries_page(after, PAGE)?;
            let Some((last, _)) = page.last() else { break };
            after = *last;
            let full = page.len() == PAGE;
            out.extend(page);
            if !full {
                break;
            }
        }
        Ok(out)
    }

    /// The current world when this engine recorded it (snapshot and replay), otherwise the newest
    /// stored state as it is (latest snapshot or replay base, no replay): for offline reports on a
    /// copy made by another engine version. Returns the world and whether it is exact.
    pub fn world_for_report(&self) -> Result<(World, bool)> {
        if self.check_engine().is_ok() {
            return Ok((self.load_world()?.0, true));
        }
        if let Some((_, v)) = self.b.snapshot_last()? {
            let row: SnapshotRow = serde_json::from_slice(&v)?;
            return Ok((row.world, false));
        }
        Ok((self.base_world()?, false))
    }

    pub fn save_snapshot(&self, w: &World) -> Result<()> {
        let row = SnapshotRow {
            hash: w.state_hash(),
            world: w.clone(),
        };
        self.b.snapshot_put(w.seq, &serde_json::to_vec(&row)?, KEEP_SNAPSHOTS)
    }

    /// The latest snapshot after the replay base, hash-verified. A snapshot at
    /// the base itself is skipped: the base world is that same state, and after
    /// `adopt_universe` the old snapshot's hash may reflect an older schema.
    pub fn latest_snapshot(&self) -> Result<Option<World>> {
        let base = self.base()?;
        let Some((seq, v)) = self.b.snapshot_last()? else { return Ok(None) };
        if seq <= base.seq {
            return Ok(None);
        }
        let row: SnapshotRow = serde_json::from_slice(&v)?;
        let actual = row.world.state_hash();
        if actual != row.hash {
            return Err(StoreError::Corrupt {
                seq,
                stored: row.hash,
                actual,
            });
        }
        Ok(Some(row.world))
    }

    /// Reconstruct the current world: latest snapshot (or the base) + journal
    /// replay with checkpoint verification. Returns the world and entries replayed.
    pub fn load_world(&self) -> Result<(World, usize)> {
        self.check_engine()?;
        let mut w = match self.latest_snapshot()? {
            Some(w) => w,
            None => self.base_world()?,
        };
        let entries = self.entries_after(w.seq)?;
        let checkpoints = self.b.checkpoints_after(w.seq)?;
        let n = entries.len();
        replay(&mut w, &entries, &checkpoints)?;
        Ok((w, n))
    }

    /// Rebuild from the replay base + full journal (ignores snapshots). Used by `verify`.
    pub fn replay_from_base(&self) -> Result<World> {
        self.check_engine()?;
        let base = self.base()?;
        let mut w = self.base_world()?;
        replay(&mut w, &self.entries_after(base.seq)?, &self.b.checkpoints_after(base.seq)?)?;
        Ok(w)
    }

    /// Rebase a universe recorded by another engine version onto this one.
    /// Requires a snapshot at the head of the journal; the old journal is kept
    /// for the record but replay starts from that snapshot from now on.
    pub fn adopt_universe(&self) -> Result<Base> {
        let old = self.base()?;
        if old.engine == ENGINE_VERSION {
            return Ok(old);
        }
        let head = self.last_seq()?;
        let snap_seq = self.b.snapshot_last_seq()?.unwrap_or(0);
        if snap_seq != head {
            return Err(StoreError::NotAtHead { snapshot: snap_seq, head });
        }
        let (_, v) = self.b.snapshot_last()?.ok_or(StoreError::NotAtHead { snapshot: 0, head })?;
        let row: SnapshotRow = serde_json::from_slice(&v)?;
        let mut history = old.history.clone();
        history.push((old.seq, old.engine.clone()));
        let base = Base {
            seq: head,
            engine: ENGINE_VERSION.into(),
            history,
        };
        self.b.meta_put(&[
            ("base", &serde_json::to_vec(&base)?),
            ("base_world", &serde_json::to_vec(&row.world)?),
        ])?;
        Ok(base)
    }
}

/// Export a Postgres universe to a portable redb file, atomically.
fn export_pg(url: &str, dest: &Path) -> Result<u64> {
    let tmp = dest.with_extension("partial");
    let _ = std::fs::remove_file(&tmp);
    let io = |e: std::io::Error| StoreError::Db(format!("backup: {e}"));
    {
        let src = Store::open_location(url, Access::Snapshot)?;
        let dst = Store::open(&tmp)?;
        src.copy_into(&dst)?;
    }
    std::fs::rename(&tmp, dest).map_err(io)?;
    Ok(std::fs::metadata(dest).map_err(io)?.len())
}

/// Every journaled entry was accepted when it was recorded, so a refusal on
/// replay means engine drift: fail loudly instead of silently skipping it.
/// Checkpoint hashes are compared as replay passes them.
fn replay(w: &mut World, entries: &[Entry], checkpoints: &[(u64, String)]) -> Result<()> {
    let mut cp = checkpoints.iter().peekable();
    for e in entries {
        if e.seq != w.seq + 1 {
            return Err(StoreError::OutOfOrder {
                expected: w.seq + 1,
                got: e.seq,
            });
        }
        apply(w, e).map_err(|r| StoreError::Replay {
            seq: e.seq,
            reason: r.to_string(),
        })?;
        while cp.peek().map(|(s, _)| *s < e.seq).unwrap_or(false) {
            cp.next();
        }
        if let Some((s, expected)) = cp.peek()
            && *s == e.seq
        {
            let got = w.state_hash();
            if &got != expected {
                return Err(StoreError::Diverged {
                    seq: e.seq,
                    expected: expected.clone(),
                    got,
                });
            }
            cp.next();
        }
    }
    Ok(())
}
