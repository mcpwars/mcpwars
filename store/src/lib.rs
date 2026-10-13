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
//! * A universe recorded by a newer engine version is always refused
//!   (`NewerEngine`): an older build would silently drop the state it does not
//!   know. Roll forward only.
//! * A universe recorded by an older engine version is refused by default.
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
pub use redb_backend::EXPORT_CACHE_BYTES;
use redb_backend::RedbBackend;

const KEEP_SNAPSHOTS: usize = 3;
/// Journal rows fetched per query when replaying or copying.
const PAGE: usize = 5_000;
/// Private archive rows fetched per query when copying. A row carries a whole note or memory
/// version, and a 5,000-row page of production's archive was about 38 MB before it was
/// re-encoded, so copies read it in smaller pages.
const PRIVATE_PAGE: usize = 500;
/// Meta keys that make up a universe (besides `schema_version`).
const UNIVERSE_KEYS: [&str; 3] = ["genesis", "base", "base_world"];

/// Write a world-hash checkpoint after every N-th journal entry.
pub const CHECKPOINT_EVERY: u64 = 100;

/// Storage layout version. Bump together with a new entry in `MIGRATIONS`.
/// Version 2 is the engine fence: it changes no table, but every build made before
/// the newer-engine refusal supports only version 1, so it refuses (`NewerFile`) a
/// database that a fenced build has opened instead of silently rebasing it.
pub const SCHEMA_VERSION: u64 = 2;

/// The oldest schema a read-only connection opens without migrating (only a writer
/// migrates). Schema 2 changed no table, so readers (verify, export, reports) still
/// open a version 1 store. Raise it with any migration that changes a table.
const READ_COMPATIBLE_FROM: u64 = 1;

type Migration = fn(&dyn Backend) -> Result<()>;
/// `MIGRATIONS[i]` upgrades a store from schema version `i + 1` to `i + 2`.
const MIGRATIONS: &[Migration] = &[engine_fence];

/// Schema 1 to 2: nothing to change. The new version number is the fence itself.
fn engine_fence(_: &dyn Backend) -> Result<()> {
    Ok(())
}
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
        "this universe was recorded by engine '{file}', newer than this binary ('{binary}'). An older build never opens or rebases a newer universe (it would drop the state the newer engine wrote): run a build at '{file}' or later (roll forward only)"
    )]
    NewerEngine { file: String, binary: String },
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

/// The deepest JSON nesting [`from_slice_deep`] parses. Stored state is bounded by construction:
/// a request body passes serde_json's default limit of 128 levels at intake, and the engine adds
/// about 9 levels of world and journal structure around a captain's value. This leaves a wide
/// margin above that and still parses on a default 2 MiB thread stack in a debug build.
pub const MAX_STORED_DEPTH: usize = 512;

static DEEPEST_READ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The deepest JSON nesting [`from_slice_deep`] has read in this process (for the boot log).
pub fn deepest_read() -> usize {
    DEEPEST_READ.load(std::sync::atomic::Ordering::Relaxed)
}

/// The maximum nesting of arrays and objects in a JSON text, counted without recursion and
/// outside strings. Invalid JSON gets a best-effort count; the parser reports the real error.
pub fn json_depth(bytes: &[u8]) -> usize {
    let (mut depth, mut max, mut in_str, mut escaped) = (0usize, 0usize, false, false);
    for &b in bytes {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'[' | b'{' => {
                depth += 1;
                max = max.max(depth);
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// Parse stored state (snapshots, the replay base, journal entries, private archive rows)
/// without serde_json's default recursion limit of 128 levels. A captain may store a value
/// that is accepted at intake but sits deeper than 128 once it is wrapped in the world or a
/// journal entry; the default limit would then fail boot, adoption and `verify`.
///
/// Stored depth is bounded by construction (intake plus about 9 levels), so this parses on the
/// caller's stack; anything deeper than [`MAX_STORED_DEPTH`] is refused with an error, never a
/// stack overflow. **Never use it for request bodies or any other outside input:** those keep
/// serde_json's default limit.
pub fn from_slice_deep<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> serde_json::Result<T> {
    let depth = json_depth(bytes);
    DEEPEST_READ.fetch_max(depth, std::sync::atomic::Ordering::Relaxed);
    if depth > MAX_STORED_DEPTH {
        return Err(<serde_json::Error as serde::de::Error>::custom(format!(
            "stored JSON nests {depth} levels, deeper than the reader's limit of {MAX_STORED_DEPTH}"
        )));
    }
    let mut de = serde_json::Deserializer::from_slice(bytes);
    de.disable_recursion_limit();
    let value = T::deserialize(&mut de)?;
    de.end()?;
    Ok(value)
}

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
                if !writer && v >= READ_COMPATIBLE_FROM {
                    return Ok(Store { b, loc });
                }
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
        self.backup_to_limited(dest, u64::MAX)
    }

    /// Online backups enforce a disk budget; offline exports may use the unlimited API.
    pub fn backup_to_limited(&self, dest: &Path, max_bytes: u64) -> Result<u64> {
        match &self.loc {
            Location::File(path) => {
                let tmp = dest.with_extension("partial");
                let _ = std::fs::remove_file(&tmp);
                let copy = || -> std::io::Result<u64> {
                    let mut from = std::fs::File::open(path)?;
                    let mut o = std::fs::OpenOptions::new();
                    o.write(true).create_new(true);
                    #[cfg(unix)]
                    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
                    let mut to = o.open(&tmp)?;
                    let n = std::io::copy(&mut std::io::Read::take(std::io::Read::by_ref(&mut from), max_bytes), &mut to)?;
                    let mut extra = [0u8; 1];
                    if std::io::Read::read(&mut from, &mut extra)? != 0 {
                        return Err(std::io::Error::other(
                            "online backup exceeds its disk budget; export on a separate machine",
                        ));
                    }
                    to.sync_all()?;
                    std::fs::rename(&tmp, dest)?;
                    Ok(n)
                };
                copy().map_err(|e| {
                    let _ = std::fs::remove_file(&tmp);
                    StoreError::Db(format!("backup: {e}"))
                })
            }
            Location::Postgres(url) => export_pg(url, dest, max_bytes),
        }
    }

    /// Create a new, empty backup or export file at `path` (redb, mode 0600 on Unix, a small page
    /// cache so writing it does not keep the copy in memory). Refuses an existing path.
    pub fn create_export(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        Self::check_schema(Box::new(RedbBackend::create_export(&path)?), Location::File(path), true)
    }

    /// Create a portable file whose allocation and writes cannot exceed `max_bytes`.
    pub fn create_export_limited(path: impl AsRef<Path>, max_bytes: u64) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        Self::check_schema(
            Box::new(RedbBackend::create_export_limited(&path, max_bytes)?),
            Location::File(path),
            true,
        )
    }

    /// A backup that can run off the writer's thread, if this store supports it.
    pub fn detached_backup(&self) -> Option<BackupJob> {
        self.detached_backup_limited(u64::MAX)
    }

    pub fn detached_backup_limited(&self, max_bytes: u64) -> Option<BackupJob> {
        match &self.loc {
            Location::File(_) => None,
            Location::Postgres(url) => {
                let url = url.clone();
                Some(Box::new(move |dest: &Path| export_pg(&url, dest, max_bytes)))
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
            limit: PRIVATE_PAGE,
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
    /// A newer engine is `NewerEngine`, any other difference `EngineMismatch`.
    pub fn check_engine(&self) -> Result<()> {
        let b = self.base()?;
        refuse_newer_engine(&b.engine)?;
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
        Ok(from_slice_deep(&v)?)
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
                let v: serde_json::Value = from_slice_deep(&data)?;
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
                out.push(from_slice_deep(v)?);
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
            .map(|(_, bytes)| from_slice_deep(&bytes).map_err(StoreError::from))
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
            let row: SnapshotRow = from_slice_deep(&v)?;
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
        let row: SnapshotRow = from_slice_deep(&v)?;
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

    /// Rebase a universe recorded by an older engine version onto this one.
    /// Requires a snapshot at the head of the journal; the old journal is kept
    /// for the record but replay starts from that snapshot from now on.
    /// A universe from a newer engine is refused (`NewerEngine`) before anything is written.
    pub fn adopt_universe(&self) -> Result<Base> {
        let old = self.base()?;
        refuse_newer_engine(&old.engine)?;
        if old.engine == ENGINE_VERSION {
            return Ok(old);
        }
        let head = self.last_seq()?;
        let snap_seq = self.b.snapshot_last_seq()?.unwrap_or(0);
        if snap_seq != head {
            return Err(StoreError::NotAtHead { snapshot: snap_seq, head });
        }
        let (_, v) = self.b.snapshot_last()?.ok_or(StoreError::NotAtHead { snapshot: 0, head })?;
        let row: SnapshotRow = from_slice_deep(&v)?;
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

/// The numbers of an engine version (`af-engine-1.41` is `[1, 41]`), or None when the
/// text is not one (an empty or hand-written name). Compared numerically, never as text.
pub fn engine_number(version: &str) -> Option<Vec<u64>> {
    let rest = version.strip_prefix("af-engine-")?;
    rest.split('.').map(|p| p.parse().ok()).collect()
}

/// True when engine version `a` is newer than `b`, comparing the numbers part by part
/// (`af-engine-1.41` is newer than `af-engine-1.9`; a missing part counts as 0).
/// A version that does not parse is never newer.
pub fn engine_newer(a: &str, b: &str) -> bool {
    let (Some(a), Some(b)) = (engine_number(a), engine_number(b)) else {
        return false;
    };
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// Refuse a universe recorded by an engine newer than this binary.
fn refuse_newer_engine(file: &str) -> Result<()> {
    if engine_newer(file, ENGINE_VERSION) {
        return Err(StoreError::NewerEngine {
            file: file.into(),
            binary: ENGINE_VERSION.into(),
        });
    }
    Ok(())
}

/// Whether this build may take over the universe at `loc`, read from its metadata
/// directly: no writer lock, no migration, no world deserialized. A starting server
/// calls it before it asks a running writer to hand over, so an older build never
/// makes a newer server let go of a universe it would then refuse to open.
/// Refuses only a newer storage schema (`NewerFile`) or a newer engine (`NewerEngine`);
/// an empty store, an older schema (the writer migrates it) and an older engine
/// (`--adopt-universe`) pass. A file location that does not exist passes too.
pub fn check_takeover(loc: &str) -> Result<()> {
    let b: Box<dyn Backend> = if pg::is_postgres(loc) {
        Box::new(PgBackend::open(loc, Access::Reader)?)
    } else if Path::new(loc).exists() {
        Box::new(RedbBackend::open(Path::new(loc))?)
    } else {
        return Ok(());
    };
    if let Some(v) = b.meta_get("schema_version")?
        && let Some(found) = serde_json::from_slice::<serde_json::Value>(&v)?.as_u64()
        && found > SCHEMA_VERSION
    {
        return Err(StoreError::NewerFile {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    if let Some(v) = b.meta_get("base")?
        && let Some(engine) = serde_json::from_slice::<serde_json::Value>(&v)?["engine"].as_str()
    {
        refuse_newer_engine(engine)?;
    }
    Ok(())
}

/// Export a universe to a new portable redb file at `dest`, atomically: it is written to
/// `dest` with the extension `partial` (a private file with a small page cache, see
/// `Store::create_export`) and renamed only when complete. A failed export removes its partial
/// file. Memory stays bounded: rows are copied a page at a time and only the latest snapshot
/// row is held, never deserialized. Callers give each export its own `dest`.
fn export(src: &Store, dest: &Path, max_bytes: u64) -> Result<u64> {
    let tmp = dest.with_extension("partial");
    let _ = std::fs::remove_file(&tmp);
    let io = |e: std::io::Error| StoreError::Db(format!("backup: {e}"));
    let copied = (|| -> Result<()> {
        let dst = Store::create_export_limited(&tmp, max_bytes)?;
        src.copy_into(&dst)?;
        Ok(())
    })();
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io(e)
    })?;
    Ok(std::fs::metadata(dest).map_err(io)?.len())
}

/// Export a Postgres universe (one repeatable-read transaction, so consistent while the writer
/// keeps going) to a portable redb file, atomically.
fn export_pg(url: &str, dest: &Path, max_bytes: u64) -> Result<u64> {
    let src = Store::open_location(url, Access::Snapshot)?;
    export(&src, dest, max_bytes)
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
