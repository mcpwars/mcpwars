//! The single-writer engine actor. Every mutation flows through one task
//! that owns the `World`, journals accepted commands durably *before*
//! replying, and fans events out to spectators.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use af_core::engine::{Actor, Entry, Event, Outcome, Refusal, apply};
use af_core::model::PlayerId;
use af_core::{Command, World};
use af_store::Store;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, mpsc, oneshot};

pub const SNAPSHOT_EVERY: u64 = 500;
const FEED_KEEP: usize = 2_000;
const JOURNAL_TAIL: usize = 1_000;

type ReadFn = Box<dyn FnOnce(&World) + Send>;

enum Req {
    Exec {
        actor: Actor,
        cmd: Command,
        reply: oneshot::Sender<Result<Outcome, Refusal>>,
    },
    Read(ReadFn),
    PrivateArchive(
        af_store::private_archive::ArchiveQuery,
        oneshot::Sender<Result<Vec<af_store::private_archive::ArchiveRow>, String>>,
    ),
    JournalPage(Option<u64>, usize, oneshot::Sender<Result<Vec<serde_json::Value>, String>>),
    Snapshot(oneshot::Sender<()>),
    /// Copy the database between transactions (a consistent online backup). The permit is held
    /// until the copy has finished, even when the requester has gone. The flag says whether a
    /// finished copy nobody waits for any more is removed (a download) or kept (a file the
    /// operator asked for on the server).
    Backup(std::path::PathBuf, BackupPermit, bool, oneshot::Sender<Result<u64, String>>),
    /// Hand the universe to a successor: snapshot at the head, close the store
    /// (which releases the writer lock), and refuse commands from then on.
    Release(oneshot::Sender<()>),
    /// Append interaction-log rows (telemetry; failures are logged, never fatal).
    Activity(Vec<af_store::ActivityRow>),
    /// Read interaction-log rows: (player or all, since_ms, limit), newest first.
    ActivityQuery(Option<PlayerId>, u64, usize, oneshot::Sender<Vec<af_store::ActivityRow>>),
    /// Activity history work on the store this thread owns (redb, memory): see `HistOp`.
    Hist(HistOp, oneshot::Sender<Option<HistOut>>),
    /// Read an operator console setting (sysop alert acknowledgements).
    ConsoleGet(String, oneshot::Sender<Option<Vec<u8>>>),
    /// Write one; refused (false) once the universe has been handed over.
    ConsolePut(String, Vec<u8>, oneshot::Sender<bool>),
}

/// Interaction-log rows waiting to be written, and when they were last flushed.
type ActivityBuf = Arc<Mutex<(Vec<af_store::ActivityRow>, std::time::Instant)>>;
/// Rows kept in memory when there is no database (tests, sims, a handed-over server).
const ACTIVITY_MEMORY: usize = 20_000;
/// Interaction-log rows kept per minute from callers who are not logged in; the rest are
/// counted and dropped, so anonymous traffic cannot flood the log.
const ANON_ROWS_PER_MINUTE: u32 = 60;

/// Work for the interaction-log thread (Postgres only: its own connection, off the engine).
enum ActivityReq {
    Append(Vec<af_store::ActivityRow>),
    Query(Option<PlayerId>, u64, usize, oneshot::Sender<Vec<af_store::ActivityRow>>),
    /// Answered once everything sent before it is written.
    Flush(oneshot::Sender<()>),
}

/// One step of activity history work (the daily rollup, its reads and the raw log's prune). Small
/// steps, so on redb the engine thread interleaves them with commands and ticks.
pub enum HistOp {
    /// Raw rows with from <= at_ms < to, oldest first.
    Between(u64, u64),
    /// The oldest raw row's time.
    Oldest,
    /// Replace one day's daily rows.
    DailyReplace(String, Vec<af_store::DailyRow>),
    /// Daily rows: from day, to day (inclusive), player, limit.
    DailyQuery(String, String, Option<u64>, usize),
    DailyDays,
    /// Delete raw rows older than this.
    Prune(u64),
}

pub enum HistOut {
    Raw(Vec<af_store::ActivityRow>),
    Oldest(Option<u64>),
    Done,
    Daily(Vec<af_store::DailyRow>),
    Days(Vec<String>),
    Pruned(u64),
}

type HistReq = (HistOp, oneshot::Sender<Option<HistOut>>);

fn hist_store(st: &Store, op: HistOp) -> af_store::Result<HistOut> {
    Ok(match op {
        HistOp::Between(a, b) => HistOut::Raw(st.activity_between(a, b)?),
        HistOp::Oldest => HistOut::Oldest(st.activity_oldest()?),
        HistOp::DailyReplace(day, rows) => {
            st.daily_replace(&day, &rows)?;
            HistOut::Done
        }
        HistOp::DailyQuery(from, to, player, limit) => HistOut::Daily(st.daily_query(&from, &to, player, limit)?),
        HistOp::DailyDays => HistOut::Days(st.daily_days()?),
        HistOp::Prune(before) => HistOut::Pruned(st.activity_prune(before)?),
    })
}

fn hist_log(log: &mut af_store::pg::ActivityLog, op: HistOp) -> af_store::Result<HistOut> {
    Ok(match op {
        HistOp::Between(a, b) => HistOut::Raw(log.between(a, b)?),
        HistOp::Oldest => HistOut::Oldest(log.oldest()?),
        HistOp::DailyReplace(day, rows) => {
            log.daily_replace(&day, &rows)?;
            HistOut::Done
        }
        HistOp::DailyQuery(from, to, player, limit) => HistOut::Daily(log.daily_query(&from, &to, player, limit)?),
        HistOp::DailyDays => HistOut::Days(log.daily_days()?),
        HistOp::Prune(before) => HistOut::Pruned(log.prune(before)?),
    })
}

/// The same work on the in-memory log (tests, sims, a server without a database).
fn hist_memory(
    raw: &mut VecDeque<af_store::ActivityRow>,
    daily: &mut std::collections::BTreeMap<(String, u64), Vec<u8>>,
    op: HistOp,
) -> HistOut {
    match op {
        HistOp::Between(a, b) => {
            let mut rows: Vec<_> = raw.iter().filter(|r| r.at_ms >= a && r.at_ms < b).cloned().collect();
            rows.sort_by_key(|r| r.at_ms);
            HistOut::Raw(rows)
        }
        HistOp::Oldest => HistOut::Oldest(raw.iter().map(|r| r.at_ms).min()),
        HistOp::DailyReplace(day, rows) => {
            daily.retain(|k, _| k.0 != day);
            for r in rows.into_iter().filter(|r| r.day == day) {
                daily.insert((r.day, r.player), r.data);
            }
            HistOut::Done
        }
        HistOp::DailyQuery(from, to, player, limit) => HistOut::Daily(
            daily
                .iter()
                .filter(|((d, p), _)| *d >= from && *d <= to && player.is_none_or(|x| x == *p))
                .take(limit)
                .map(|((d, p), v)| af_store::DailyRow {
                    day: d.clone(),
                    player: *p,
                    data: v.clone(),
                })
                .collect(),
        ),
        HistOp::DailyDays => {
            let mut days: Vec<String> = daily.keys().map(|k| k.0.clone()).collect();
            days.dedup();
            HistOut::Days(days)
        }
        HistOp::Prune(before) => {
            let n = raw.len();
            raw.retain(|r| r.at_ms >= before);
            HistOut::Pruned((n - raw.len()) as u64)
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct FeedItem {
    pub id: u64,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Clone)]
pub struct Game {
    tx: mpsc::Sender<Req>,
    /// Live event feed (sysop).
    pub feed: broadcast::Sender<FeedItem>,
    pub recent: Arc<Mutex<VecDeque<FeedItem>>>,
    /// Public event feed: live, but only the events `af_core::public` allows, without positions.
    pub public_feed: broadcast::Sender<FeedItem>,
    pub public_recent: Arc<Mutex<VecDeque<FeedItem>>>,
    /// The most recent accepted journal entries (sysop audit log).
    pub journal_tail: Arc<Mutex<VecDeque<Entry>>>,
    pub paused: Arc<std::sync::atomic::AtomicBool>,
    /// Evaluation mode only: every accepted entry, in order (the episode journal).
    pub recording: Option<Arc<Mutex<Vec<Entry>>>>,
    /// Self-service registration limits (REST and join_game share them).
    pub registration: Arc<RegistrationPolicy>,
    /// Per-IP limit on anonymous REST feedback (the client IP comes from `registration`).
    pub feedback_limit: Arc<RegistrationPolicy>,
    activity: ActivityBuf,
    /// The interaction-log thread on Postgres; None writes through the engine (redb, memory).
    activity_worker: Option<std::sync::mpsc::SyncSender<ActivityReq>>,
    /// The activity history thread on Postgres (its own connection, so a long rollup never
    /// delays the log); None does that work on the engine thread (redb, memory).
    history_worker: Option<std::sync::mpsc::Sender<HistReq>>,
    /// Set once the universe is handed over: no more rollups or prunes from this server.
    released: Arc<std::sync::atomic::AtomicBool>,
    /// Anonymous rows this minute: (minute, kept, dropped).
    anon_rows: Arc<Mutex<(u64, u32, u32)>>,
    /// The public fleet frames, served an hour late (see `fleet`).
    pub fleet: crate::fleet::FleetRing,
    /// The scoreboard's hourly trend and the finished seasons (see `history`).
    pub history: crate::history::HistoryState,
    /// Set while a backup runs (see `backup_permit`).
    backup_busy: Arc<std::sync::atomic::AtomicBool>,
}

/// The refusal when a backup is asked for while another one runs.
pub const BACKUP_BUSY: &str = "A backup is already running on this server; try again when it has finished.";

/// The right to run the one backup this server allows at a time. Cloned between the requester
/// (which may keep it while it streams the file) and the job (which keeps it until the copy has
/// finished); the slot frees when the last clone is dropped.
#[derive(Clone)]
pub struct BackupPermit {
    _slot: Arc<BackupSlot>,
}

struct BackupSlot(Arc<std::sync::atomic::AtomicBool>);

impl Drop for BackupSlot {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Reply to a finished backup. For a download (`discard`), nobody waiting (an HTTP client that
/// went away) means nobody will use the file, so it is removed rather than left on disk.
fn finish_backup(dest: &std::path::Path, discard: bool, done: Option<oneshot::Sender<Result<u64, String>>>, r: Result<u64, String>) {
    let ok = r.is_ok();
    let delivered = done.is_some_and(|d| d.send(r).is_ok());
    if ok && discard && !delivered {
        let _ = std::fs::remove_file(dest);
    }
}

/// Anonymous REST feedback reports allowed per client IP per rolling hour.
pub const FEEDBACK_PER_IP_PER_HOUR: u32 = 20;

/// Per-client-IP limit on self-service registration.
pub struct RegistrationPolicy {
    /// Registrations allowed per IP per rolling hour (0 = unlimited).
    pub per_ip_per_hour: std::sync::atomic::AtomicU32,
    /// Header carrying the real client IP when behind a trusted proxy (e.g. "fly-client-ip").
    pub trusted_ip_header: Mutex<Option<String>>,
    recent: Mutex<std::collections::BTreeMap<String, VecDeque<std::time::Instant>>>,
}

impl Default for RegistrationPolicy {
    fn default() -> Self {
        RegistrationPolicy {
            per_ip_per_hour: 10.into(),
            trusted_ip_header: Mutex::new(None),
            recent: Mutex::new(Default::default()),
        }
    }
}

impl RegistrationPolicy {
    /// A policy allowing `per_hour` attempts per IP per rolling hour (0 = unlimited).
    pub fn per_hour(per_hour: u32) -> Self {
        RegistrationPolicy {
            per_ip_per_hour: per_hour.into(),
            ..Default::default()
        }
    }

    /// The client IP: the trusted proxy header if configured and present, else the socket peer.
    pub fn client_ip(&self, headers: &http::HeaderMap, peer: Option<std::net::SocketAddr>) -> String {
        if let Some(h) = self.trusted_ip_header.lock().unwrap().as_deref()
            && let Some(v) = headers.get(h).and_then(|v| v.to_str().ok())
        {
            return v.split(',').next().unwrap_or("").trim().to_string();
        }
        peer.map(|p| p.ip().to_string()).unwrap_or_else(|| "unknown".into())
    }

    /// Record an attempt; false if this IP is over its hourly limit.
    pub fn allow(&self, ip: &str) -> bool {
        let limit = self.per_ip_per_hour.load(std::sync::atomic::Ordering::Relaxed) as usize;
        if limit == 0 {
            return true;
        }
        let now = std::time::Instant::now();
        let mut m = self.recent.lock().unwrap();
        let q = m.entry(ip.to_string()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t).as_secs() >= 3_600) {
            q.pop_front();
        }
        if q.len() >= limit {
            return false;
        }
        q.push_back(now);
        if m.len() > 50_000 {
            m.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t).as_secs() < 3_600));
        }
        true
    }
}

fn push_feed(ring: &Mutex<VecDeque<FeedItem>>, tx: &broadcast::Sender<FeedItem>, item: FeedItem) {
    {
        let mut r = ring.lock().unwrap();
        r.push_back(item.clone());
        while r.len() > FEED_KEEP {
            r.pop_front();
        }
    }
    let _ = tx.send(item);
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.trim().as_bytes()))
}

/// A captain's recovery key: long-lived, stored privately by the agent, used only to mint a
/// new token if the token is lost.
pub fn new_recovery_key() -> String {
    format!("afr_{}", &new_token()[3..])
}

pub fn new_token() -> String {
    let mut b = [0u8; 24];
    getrandom::fill(&mut b).expect("os randomness");
    format!("af_{}", hex::encode(b))
}

impl Game {
    /// Spawn the actor. `store` = None runs purely in memory (tests, sims).
    pub fn spawn(world: World, store: Option<Store>) -> Game {
        Self::spawn_inner(world, store, None)
    }

    /// Evaluation mode: in memory, no ticker, a synthetic clock (`at_ms` = tick x
    /// `tick_ms`, so identical inputs give identical journals and state hashes),
    /// and every accepted entry recorded in `recording`.
    pub fn spawn_eval(world: World) -> Game {
        let tick_ms = world.rules.tick_ms.max(1);
        Self::spawn_inner(world, None, Some(tick_ms))
    }

    fn spawn_inner(world: World, store: Option<Store>, synthetic_tick_ms: Option<u64>) -> Game {
        let (tx, mut rx) = mpsc::channel::<Req>(1024);
        let (feed, _) = broadcast::channel(4096);
        let (public_feed, _) = broadcast::channel(4096);
        let recent = Arc::new(Mutex::new(VecDeque::new()));
        let public_recent = Arc::new(Mutex::new(VecDeque::new()));
        let journal_tail = Arc::new(Mutex::new(VecDeque::new()));
        let game = Game {
            tx,
            feed: feed.clone(),
            recent: recent.clone(),
            public_feed: public_feed.clone(),
            public_recent: public_recent.clone(),
            journal_tail: journal_tail.clone(),
            paused: Arc::new(false.into()),
            recording: synthetic_tick_ms.map(|_| Arc::new(Mutex::new(Vec::new()))),
            registration: Arc::new(RegistrationPolicy::default()),
            feedback_limit: Arc::new(RegistrationPolicy::per_hour(FEEDBACK_PER_IP_PER_HOUR)),
            activity: Arc::new(Mutex::new((Vec::new(), std::time::Instant::now()))),
            activity_worker: store.as_ref().and_then(|s| s.activity_log()).map(spawn_activity_worker),
            history_worker: store.as_ref().and_then(|s| s.activity_log()).map(spawn_history_worker),
            released: Arc::new(false.into()),
            anon_rows: Arc::new(Mutex::new((0, 0, 0))),
            fleet: crate::fleet::ring(),
            history: crate::history::state(),
            backup_busy: Arc::new(false.into()),
        };
        let recording = game.recording.clone();
        // The store clients are synchronous: run the actor on a dedicated thread.
        std::thread::Builder::new()
            .name("af-engine".into())
            .spawn(move || {
                let mut w = world;
                let mut store = store;
                if let Some(st) = &store {
                    st.backfill_private(&w).expect("permanent private archive backfill");
                }
                let mut private_mem: Vec<af_store::private_archive::ArchiveRow> = vec![];
                let mut released = false;
                let mut since_snapshot = 0u64;
                let mut feed_id = 0u64;
                let mut public_id = 0u64;
                let mut activity_mem: VecDeque<af_store::ActivityRow> = VecDeque::new();
                let mut daily_mem: std::collections::BTreeMap<(String, u64), Vec<u8>> = Default::default();
                let mut console_mem: std::collections::BTreeMap<String, Vec<u8>> = Default::default();
                while let Some(req) = rx.blocking_recv() {
                    match req {
                        Req::Activity(rows) => match &store {
                            // Pruned only by the daily rollup, after it has summarised the days it removes.
                            Some(st) => {
                                if let Err(e) = st.activity_append(&rows) {
                                    tracing::warn!("activity log append failed ({} rows dropped): {e}", rows.len());
                                }
                            }
                            None => {
                                activity_mem.extend(rows);
                                while activity_mem.len() > ACTIVITY_MEMORY {
                                    activity_mem.pop_front();
                                }
                            }
                        },
                        Req::Hist(op, reply) => {
                            let out = match &store {
                                Some(st) => hist_store(st, op).map_err(|e| tracing::warn!("activity history: {e}")).ok(),
                                None if released => None,
                                None => Some(hist_memory(&mut activity_mem, &mut daily_mem, op)),
                            };
                            let _ = reply.send(out);
                        }
                        Req::ActivityQuery(player, since, limit, reply) => {
                            let rows = match &store {
                                Some(st) => st.activity(player.map(u64::from), since, limit).unwrap_or_else(|e| {
                                    tracing::warn!("activity log query failed: {e}");
                                    vec![]
                                }),
                                None => activity_mem
                                    .iter()
                                    .rev()
                                    .filter(|r| r.at_ms >= since && (player.is_none() || r.player == player.map(u64::from)))
                                    .take(limit)
                                    .cloned()
                                    .collect(),
                            };
                            let _ = reply.send(rows);
                        }
                        Req::ConsoleGet(key, reply) => {
                            let v = match &store {
                                Some(st) => st.console_get(&key).unwrap_or_else(|e| {
                                    tracing::warn!("console setting read failed: {e}");
                                    None
                                }),
                                None => console_mem.get(&key).cloned(),
                            };
                            let _ = reply.send(v);
                        }
                        Req::ConsolePut(key, value, reply) => {
                            let ok = !released
                                && match &store {
                                    Some(st) => st
                                        .console_put(&key, &value)
                                        .map_err(|e| tracing::warn!("console setting write failed: {e}"))
                                        .is_ok(),
                                    None => {
                                        console_mem.insert(key, value);
                                        true
                                    }
                                };
                            let _ = reply.send(ok);
                        }
                        Req::JournalPage(before, limit, reply) => {
                            let result = match &store {
                                Some(st) => st.journal_page(before, limit).map_err(|e| e.to_string()),
                                None if released => Err("This server handed over; reconnect to the active server.".into()),
                                None => journal_tail
                                    .lock()
                                    .unwrap()
                                    .iter()
                                    .rev()
                                    .filter(|e| before.is_none_or(|b| e.seq < b))
                                    .take(limit.min(1_001))
                                    .map(|e| serde_json::to_value(e).map_err(|e| e.to_string()))
                                    .collect(),
                            };
                            let _ = reply.send(result);
                        }
                        Req::PrivateArchive(q, reply) => {
                            let result = match &store {
                                Some(st) => st.private_archive(&q).map_err(|e| e.to_string()),
                                None if released => Err("This server handed over; reconnect to the active server.".into()),
                                None => Ok(private_mem
                                    .iter()
                                    .filter(|r| {
                                        (r.seq, r.ordinal) > (q.after_seq, q.after_ordinal)
                                            && q.player.is_none_or(|p| p == r.player)
                                            && q.kind.as_ref().is_none_or(|k| *k == r.kind)
                                            && q.key.as_ref().is_none_or(|k| *k == r.key)
                                    })
                                    .take(q.limit)
                                    .cloned()
                                    .collect()),
                            };
                            let _ = reply.send(result);
                        }
                        Req::Read(f) => f(&w),
                        Req::Backup(dest, permit, discard, done) => match store.as_ref().map(|st| st.detached_backup()) {
                            // Postgres: a consistent export on its own connection, off this thread.
                            Some(Some(job)) => {
                                let (path, failed) = (dest.clone(), Arc::new(Mutex::new(Some(done))));
                                let reply = failed.clone();
                                let spawned = std::thread::Builder::new().name("af-backup".into()).spawn(move || {
                                    // A panic in the copy is an error reply, never a dead process.
                                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&path)))
                                        .unwrap_or_else(|_| Err(af_store::StoreError::Db("backup: the copy panicked".into())))
                                        .map_err(|e| e.to_string());
                                    finish_backup(&path, discard, reply.lock().unwrap().take(), r);
                                    drop(permit);
                                });
                                if let Err(e) = spawned
                                    && let Some(done) = failed.lock().unwrap().take()
                                {
                                    let _ = done.send(Err(format!("backup: could not start the copy: {e}")));
                                }
                            }
                            Some(None) => {
                                let r = store.as_ref().unwrap().backup_to(&dest).map_err(|e| e.to_string());
                                finish_backup(&dest, discard, Some(done), r);
                                drop(permit);
                            }
                            None => {
                                let _ = done.send(Err("this universe has no database (in-memory or handed over)".to_string()));
                            }
                        },
                        Req::Release(done) => {
                            if let Some(st) = store.take() {
                                if let Err(e) = st.save_snapshot(&w) {
                                    tracing::error!("handover snapshot failed: {e}");
                                }
                                drop(st);
                            }
                            released = true;
                            let _ = done.send(());
                        }
                        Req::Snapshot(done) => {
                            if let Some(st) = &store
                                && let Err(e) = st.save_snapshot(&w)
                            {
                                tracing::error!("snapshot failed: {e}");
                            }
                            let _ = done.send(());
                        }
                        Req::Exec { reply, .. } if released => {
                            let _ = reply.send(Err(Refusal::new(
                                "server_moving",
                                "This server is handing over to a new version. Retry in a few seconds.",
                            )));
                        }
                        Req::Exec { actor, cmd, reply } => {
                            let (prev_seq, prev_now) = (w.seq, w.now_ms);
                            let e = Entry {
                                seq: prev_seq + 1,
                                at_ms: synthetic_tick_ms.map(|ms| w.tick * ms).unwrap_or_else(now_ms),
                                actor,
                                cmd,
                            };
                            let private_before = af_store::private_archive::Capture::before(&w, &e);
                            match apply(&mut w, &e) {
                                Ok(out) => {
                                    let private_rows = private_before.after(&w, &e).expect("serialize private archive");
                                    if let Some(st) = &store {
                                        let cp = e.seq.is_multiple_of(af_store::CHECKPOINT_EVERY).then(|| w.state_hash());
                                        if let Err(err) = st.append_with_private(&e, cp.as_deref(), &private_rows) {
                                            // Fail-stop: never acknowledge state we could not persist.
                                            tracing::error!("FATAL: journal append failed: {err}");
                                            eprintln!("FATAL: journal append failed: {err}");
                                            std::process::exit(2);
                                        }
                                        since_snapshot += 1;
                                        if since_snapshot >= SNAPSHOT_EVERY {
                                            since_snapshot = 0;
                                            if let Err(err) = st.save_snapshot(&w) {
                                                tracing::error!("snapshot failed: {err}");
                                            }
                                        }
                                    }
                                    if store.is_none() {
                                        private_mem.extend(private_rows);
                                    }
                                    for ev in &out.events {
                                        feed_id += 1;
                                        push_feed(
                                            &recent,
                                            &feed,
                                            FeedItem {
                                                id: feed_id,
                                                event: ev.clone(),
                                            },
                                        );
                                        if let Some(public) = af_core::public::event(ev) {
                                            public_id += 1;
                                            push_feed(
                                                &public_recent,
                                                &public_feed,
                                                FeedItem {
                                                    id: public_id,
                                                    event: public,
                                                },
                                            );
                                        }
                                    }
                                    {
                                        let mut t = journal_tail.lock().unwrap();
                                        t.push_back(e.clone());
                                        while t.len() > JOURNAL_TAIL {
                                            t.pop_front();
                                        }
                                    }
                                    if let Some(rec) = &recording {
                                        rec.lock().unwrap().push(e.clone());
                                    }
                                    let _ = reply.send(Ok(out));
                                }
                                Err(r) => {
                                    w.seq = prev_seq;
                                    w.now_ms = prev_now;
                                    let _ = reply.send(Err(r));
                                }
                            }
                        }
                    }
                }
                if let Some(st) = &store {
                    let _ = st.save_snapshot(&w);
                }
            })
            .expect("spawn engine thread");
        game
    }

    pub async fn exec(&self, actor: Actor, cmd: Command) -> Result<Outcome, Refusal> {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Req::Exec { actor, cmd, reply }).await.is_err() {
            return Err(Refusal::new("server_down", "The game engine is not running."));
        }
        rx.await
            .unwrap_or_else(|_| Err(Refusal::new("server_down", "The game engine stopped.")))
    }

    pub async fn read<T: Send + 'static>(&self, f: impl FnOnce(&World) -> T + Send + 'static) -> T {
        let (reply, rx) = oneshot::channel();
        let job: ReadFn = Box::new(move |w: &World| {
            let _ = reply.send(f(w));
        });
        self.tx.send(Req::Read(job)).await.expect("engine running");
        rx.await.expect("engine replied")
    }

    pub async fn private_archive(
        &self,
        q: af_store::private_archive::ArchiveQuery,
    ) -> Result<Vec<af_store::private_archive::ArchiveRow>, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Req::PrivateArchive(q, tx))
            .await
            .map_err(|_| "Engine stopped".to_string())?;
        rx.await.map_err(|_| "Engine stopped".to_string())?
    }

    /// The permit to run a backup, or None while another backup runs (one at a time: each
    /// holds a copy of the universe in flight, and production has little memory to spare).
    pub fn backup_permit(&self) -> Option<BackupPermit> {
        use std::sync::atomic::Ordering;
        self.backup_busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| BackupPermit {
                _slot: Arc::new(BackupSlot(self.backup_busy.clone())),
            })
    }

    /// Write a consistent copy of the database to `dest` (bytes written), or refuse with
    /// `BACKUP_BUSY` while another backup runs.
    pub async fn backup(&self, dest: std::path::PathBuf) -> Result<u64, String> {
        let permit = self.backup_permit().ok_or_else(|| BACKUP_BUSY.to_string())?;
        self.backup_with(dest, permit, false).await
    }

    /// `backup` under a permit the caller already holds. `dest` must be a path no other backup
    /// uses. If the caller stops waiting, the copy still finishes (under the permit); its file is
    /// then removed when `discard` is set (a download nobody will receive) and kept otherwise
    /// (the `backup` action's file on the server).
    pub async fn backup_with(&self, dest: std::path::PathBuf, permit: BackupPermit, discard: bool) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Req::Backup(dest, permit, discard, tx))
            .await
            .map_err(|_| "engine not running".to_string())?;
        rx.await.map_err(|_| "engine stopped".to_string())?
    }

    /// Hand the universe over to a successor (see `Req::Release`).
    pub async fn release(&self) {
        self.released.store(true, std::sync::atomic::Ordering::SeqCst);
        self.flush_activity().await;
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Req::Release(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }

    pub async fn snapshot(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Req::Snapshot(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }

    /// Log one interaction. Buffered; written every few seconds (see `start_activity_flush`)
    /// or every 64 rows. Anonymous rows are capped per minute.
    pub fn record(&self, c: crate::activity::Call) {
        if c.player.is_none() {
            let minute = c.at_ms / 60_000;
            let mut a = self.anon_rows.lock().unwrap_or_else(|p| p.into_inner());
            if a.0 != minute {
                if a.2 > 0 {
                    tracing::warn!(
                        "activity log: {} anonymous rows over the {ANON_ROWS_PER_MINUTE}/minute cap dropped",
                        a.2
                    );
                }
                *a = (minute, 0, 0);
            }
            if a.1 >= ANON_ROWS_PER_MINUTE {
                a.2 += 1;
                return;
            }
            a.1 += 1;
        }
        let row = af_store::ActivityRow {
            at_ms: c.at_ms,
            player: c.player.map(u64::from),
            data: serde_json::to_vec(&c).unwrap_or_default(),
        };
        let mut b = self.activity.lock().unwrap_or_else(|p| p.into_inner());
        b.0.push(row);
        if b.0.len() >= 64 || b.1.elapsed() > Duration::from_secs(3) {
            let rows = std::mem::take(&mut b.0);
            b.1 = std::time::Instant::now();
            drop(b);
            self.send_rows(rows);
        }
    }

    /// Hand rows to the log writer without waiting (dropped, with a warning, if it is backed up).
    fn send_rows(&self, rows: Vec<af_store::ActivityRow>) {
        let sent = match &self.activity_worker {
            Some(w) => w.try_send(ActivityReq::Append(rows)).is_ok(),
            None => self.tx.try_send(Req::Activity(rows)).is_ok(),
        };
        if !sent {
            tracing::warn!("activity log: writer busy, rows dropped");
        }
    }

    /// Flush the interaction log every few seconds, so the last calls of a quiet session are
    /// written even when nothing else arrives.
    pub fn start_activity_flush(&self) {
        let g = self.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(3));
            loop {
                iv.tick().await;
                g.flush_activity().await;
            }
        });
    }

    /// Write any buffered interaction-log rows now.
    pub async fn flush_activity(&self) {
        let rows = {
            let mut b = self.activity.lock().unwrap_or_else(|p| p.into_inner());
            b.1 = std::time::Instant::now();
            std::mem::take(&mut b.0)
        };
        match &self.activity_worker {
            Some(w) => {
                if !rows.is_empty() && w.try_send(ActivityReq::Append(rows)).is_err() {
                    tracing::warn!("activity log: writer busy, rows dropped");
                }
                // Wait (briefly) until the worker has written everything sent so far.
                let (tx, rx) = oneshot::channel();
                if w.try_send(ActivityReq::Flush(tx)).is_ok() {
                    let _ = tokio::time::timeout(Duration::from_secs(5), rx).await;
                }
            }
            None => {
                if !rows.is_empty() {
                    let _ = self.tx.send(Req::Activity(rows)).await;
                }
            }
        }
    }

    /// Logged interactions since `since_ms`, newest first (one captain, or everyone).
    /// An operator console setting (JSON), if one was saved.
    pub async fn console_get(&self, key: &str) -> Option<serde_json::Value> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Req::ConsoleGet(key.to_string(), tx)).await.ok()?;
        rx.await.ok().flatten().and_then(|b| serde_json::from_slice(&b).ok())
    }

    /// Save an operator console setting; false if it could not be stored.
    pub async fn console_put(&self, key: &str, value: &serde_json::Value) -> bool {
        let (tx, rx) = oneshot::channel();
        let Ok(bytes) = serde_json::to_vec(value) else { return false };
        if self.tx.send(Req::ConsolePut(key.to_string(), bytes, tx)).await.is_err() {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Run one step of activity history work where this server keeps the log; None if it failed
    /// (logged) or this server has handed the universe over.
    pub async fn hist(&self, op: HistOp) -> Option<HistOut> {
        if self.released.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        let (tx, rx) = oneshot::channel();
        let sent = match &self.history_worker {
            Some(w) => w.send((op, tx)).is_ok(),
            None => self.tx.send(Req::Hist(op, tx)).await.is_ok(),
        };
        if !sent {
            return None;
        }
        rx.await.ok().flatten()
    }

    /// Whether this server has handed the universe over.
    pub fn is_released(&self) -> bool {
        self.released.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub async fn journal_page(&self, before: Option<u64>, limit: usize) -> Result<Vec<serde_json::Value>, String> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Req::JournalPage(before, limit, tx)).await.map_err(|e| e.to_string())?;
        rx.await.map_err(|e| e.to_string())?
    }

    pub async fn activity(&self, player: Option<PlayerId>, since_ms: u64, limit: usize) -> Vec<crate::activity::Call> {
        self.flush_activity().await;
        let (tx, rx) = oneshot::channel();
        let sent = match &self.activity_worker {
            Some(w) => w.try_send(ActivityReq::Query(player, since_ms, limit, tx)).is_ok(),
            None => self.tx.send(Req::ActivityQuery(player, since_ms, limit, tx)).await.is_ok(),
        };
        if !sent {
            return vec![];
        }
        rx.await
            .unwrap_or_default()
            .iter()
            .filter_map(|r| serde_json::from_slice(&r.data).ok())
            .collect()
    }

    pub async fn auth(&self, token: &str) -> Option<PlayerId> {
        if token.trim().is_empty() {
            return None;
        }
        let h = hash_token(token);
        self.read(move |w| w.players.values().find(|p| !p.banned && p.token_hash == h).map(|p| p.id))
            .await
    }

    /// Resolve a token to (player, is_owner): a captain token or an owner token.
    pub async fn auth_role(&self, token: &str) -> Option<(PlayerId, bool)> {
        if token.trim().is_empty() {
            return None;
        }
        let h = hash_token(token);
        self.read(move |w| {
            // A banned captain's tokens (captain and owner) let nobody in.
            w.players.values().filter(|p| !p.banned).find_map(|p| {
                if p.token_hash == h {
                    Some((p.id, false))
                } else if !p.owner_token_hash.is_empty() && p.owner_token_hash == h {
                    Some((p.id, true))
                } else {
                    None
                }
            })
        })
        .await
    }

    /// Register a new player and return (player id, raw token).
    pub async fn register(
        &self,
        name: &str,
        model: Option<String>,
        kind: af_core::model::PlayerKind,
    ) -> Result<(PlayerId, String), Refusal> {
        self.register_full(name, model, None, false, kind)
            .await
            .map(|(id, t, _, _)| (id, t))
    }

    /// Register a new player and return (player id, captain token, owner token).
    /// Refuse a model string that is a local path, repository or build name rather than the
    /// model's public name. Checked here, not in the engine, so old journals still replay.
    pub fn check_model(model: &Option<String>) -> Result<(), Refusal> {
        let Some(m) = model.as_deref().map(str::trim).filter(|m| !m.is_empty()) else {
            return Ok(());
        };
        let lower = m.to_ascii_lowercase();
        let build_tag = ["4bit", "8bit", "gguf", "mlx", "awq", "gptq", "safetensors", "q4_", "q8_"]
            .iter()
            .any(|t| lower.contains(t));
        if m.len() > 40 || m.contains(['/', '\\', ' ']) || build_tag {
            return Err(Refusal::new(
                "bad_model_name",
                format!("\"{m}\" is not a model's public name."),
            )
            .hint("Send the model's public name as its maker publishes it, short and lowercase, e.g. \"gemma-4-26b\", \"claude-haiku-4-5\", \"gpt-6-astra\": no local path, repository, provider prefix, quantization or build tag."));
        }
        Ok(())
    }

    pub async fn register_full(
        &self,
        name: &str,
        model: Option<String>,
        harness: Option<String>,
        autonomous: bool,
        kind: af_core::model::PlayerKind,
    ) -> Result<(PlayerId, String, String, String), Refusal> {
        Self::check_model(&model)?;
        let token = new_token();
        let owner = new_token();
        let recovery = new_recovery_key();
        let args = af_core::cmd::RegisterArgs {
            name: name.to_string(),
            kind,
            token_hash: hash_token(&token),
            model,
            owner_token_hash: Some(hash_token(&owner)),
            harness,
            autonomous,
            recovery_hash: Some(hash_token(&recovery)),
        };
        let out = match self.exec(Actor::System, Command::Register(args)).await {
            Err(r) if r.code == "name_taken" || r.code == "name_overused" => {
                return Err(r.hint(format!(
                    "Invent a different name for the character you will play: invented, never a real person's. A fresh random spark: {}.",
                    crate::callsigns::spark()
                )));
            }
            other => other?,
        };
        let id = out.reply["player_id"].as_u64().unwrap_or(0) as PlayerId;
        Ok((id, token, owner, recovery))
    }

    /// Replace a captain's lost token using its recovery key; returns (player, new token).
    pub async fn recover(&self, player: &str, recovery_key: &str) -> Result<(PlayerId, String), Refusal> {
        let token = new_token();
        let out = self
            .exec(
                Actor::System,
                Command::Recover(af_core::cmd::RecoverArgs {
                    player: player.trim().to_string(),
                    recovery_hash: hash_token(recovery_key.trim()),
                    new_token_hash: hash_token(&token),
                }),
            )
            .await?;
        Ok((out.reply["player_id"].as_u64().unwrap_or(0) as PlayerId, token))
    }

    /// Spawn the wall-clock ticker.
    pub fn start_ticker(&self, tick: Duration) {
        let g = self.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(tick);
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            iv.tick().await;
            loop {
                iv.tick().await;
                if g.paused.load(std::sync::atomic::Ordering::Relaxed) {
                    continue;
                }
                if let Err(e) = g.exec(Actor::System, Command::Tick).await
                    && e.code != "server_moving"
                {
                    tracing::warn!("tick refused: {e}");
                }
            }
        });
    }
}

/// The interaction-log thread: writes, reads and prunes on its own Postgres connection, so slow
/// telemetry never delays commands or ticks. Failures are logged and the rows dropped.
fn spawn_activity_worker(mut log: af_store::pg::ActivityLog) -> std::sync::mpsc::SyncSender<ActivityReq> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<ActivityReq>(256);
    std::thread::Builder::new()
        .name("af-activity".into())
        .spawn(move || {
            while let Ok(req) = rx.recv() {
                match req {
                    // Pruned only by the daily rollup, after it has summarised the days it removes.
                    ActivityReq::Append(rows) => {
                        if let Err(e) = log.append(&rows) {
                            tracing::warn!("activity log append failed ({} rows dropped): {e}", rows.len());
                        }
                    }
                    ActivityReq::Query(player, since, limit, reply) => {
                        let rows = log.query(player.map(u64::from), since, limit).unwrap_or_else(|e| {
                            tracing::warn!("activity log query failed: {e}");
                            vec![]
                        });
                        let _ = reply.send(rows);
                    }
                    ActivityReq::Flush(reply) => {
                        let _ = reply.send(());
                    }
                }
            }
        })
        .expect("spawn activity thread");
    tx
}

/// The activity history thread (Postgres only): the daily rollup, its reads and the prune, on a
/// connection of its own, so a long first rollup never delays the interaction log or the engine.
fn spawn_history_worker(mut log: af_store::pg::ActivityLog) -> std::sync::mpsc::Sender<HistReq> {
    let (tx, rx) = std::sync::mpsc::channel::<HistReq>();
    std::thread::Builder::new()
        .name("af-history".into())
        .spawn(move || {
            while let Ok((op, reply)) = rx.recv() {
                let out = hist_log(&mut log, op).map_err(|e| tracing::warn!("activity history: {e}")).ok();
                let _ = reply.send(out);
            }
        })
        .expect("spawn history thread");
    tx
}

/// What a captain must keep, told at every registration and recovery.
pub const PERSIST_NOTE: &str = "Persist privately: token (every check-in) and recovery_key (only to replace a lost token, with recover_access), plus your call sign and player_id. Never print them or a URL that contains the token. Registration is not complete until you have read both back from your store and reconnected with the token.";

#[cfg(test)]
mod tests {
    use super::Game;

    #[test]
    fn model_names() {
        for ok in ["claude-haiku-4-5", "gemma-4-26b", "grok-4.7", "gpt-6-astra", "llama3.1:8b", ""] {
            assert!(Game::check_model(&Some(ok.into())).is_ok(), "{ok}");
        }
        assert!(Game::check_model(&None).is_ok());
        for bad in [
            "dev-gemma/mlx-community/gemma-4-26B-A4B-it-heretic-4bit",
            "anthropic/claude-haiku-4-5",
            "gemma-4-26b-it-Q4_K_M",
            "/Users/me/models/gemma",
        ] {
            assert!(Game::check_model(&Some(bad.into())).is_err(), "{bad}");
        }
    }
}
