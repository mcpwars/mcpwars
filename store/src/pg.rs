//! Postgres backend (hosted production). Same tables as the embedded file, in
//! the connection's current schema, so one database can hold several universes.
//!
//! Single writer: a writer holds a session-level advisory lock on its one
//! connection for its whole life, and every write goes through that connection.
//! If the connection drops, Postgres releases the lock and every later write on
//! it fails, so the server fail-stops instead of writing alongside a successor.
//! A starting server that finds the lock held asks the holder to hand over
//! (`NOTIFY af_handoff`); the holder snapshots, releases and goes quiet.

use std::sync::Mutex;
use std::time::Duration;

use postgres::config::SslMode;
use postgres::{Client, Config, NoTls};

use crate::backend::{ActivityRow, Backend, DailyRow, Table};
use crate::{Result, StoreError};

/// Notification channel a starting server uses to ask the writer to hand over.
const HANDOFF_CHANNEL: &str = "af_handoff";

const SCHEMA_SQL: &str = "
SET client_min_messages = warning;
CREATE TABLE IF NOT EXISTS af_private_archive (seq BIGINT NOT NULL, ordinal BIGINT NOT NULL, player BIGINT NOT NULL, kind TEXT NOT NULL, key TEXT NOT NULL, row_data BYTEA NOT NULL, PRIMARY KEY (seq, ordinal));
CREATE INDEX IF NOT EXISTS af_private_archive_player ON af_private_archive (player, seq, ordinal);
CREATE INDEX IF NOT EXISTS af_private_archive_key ON af_private_archive (player, kind, key, seq, ordinal);
CREATE INDEX IF NOT EXISTS af_private_archive_key_only ON af_private_archive (player, key, seq, ordinal);
CREATE TABLE IF NOT EXISTS af_meta (key TEXT PRIMARY KEY, value BYTEA NOT NULL);
CREATE TABLE IF NOT EXISTS af_journal (seq BIGINT PRIMARY KEY, entry BYTEA NOT NULL);
CREATE TABLE IF NOT EXISTS af_checkpoints (seq BIGINT PRIMARY KEY, hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS af_snapshots (seq BIGINT PRIMARY KEY, row_data BYTEA NOT NULL);
CREATE TABLE IF NOT EXISTS af_activity (id BIGSERIAL PRIMARY KEY, at_ms BIGINT NOT NULL, player BIGINT, row_data BYTEA NOT NULL);
CREATE INDEX IF NOT EXISTS af_activity_at ON af_activity (at_ms);
CREATE INDEX IF NOT EXISTS af_activity_player ON af_activity (player, at_ms);
CREATE TABLE IF NOT EXISTS af_activity_daily (day TEXT NOT NULL, player BIGINT NOT NULL, row_data BYTEA NOT NULL, PRIMARY KEY (day, player));
";

fn private_insert(tx: &mut postgres::Transaction<'_>, rows: &[crate::private_archive::ArchiveRow]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let stmt=tx.prepare("INSERT INTO af_private_archive (seq,ordinal,player,kind,key,row_data) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (seq,ordinal) DO NOTHING")?;
    for r in rows {
        tx.execute(
            &stmt,
            &[&seq_i64(r.seq), &seq_i64(r.ordinal), &seq_i64(r.player), &r.kind, &r.key, &r.data],
        )?;
    }
    Ok(())
}

/// SQL shared by the writer's connection and the interaction-log connections.
mod sql {
    pub const BETWEEN: &str = "SELECT at_ms, player, row_data FROM af_activity WHERE at_ms >= $1 AND at_ms < $2 ORDER BY at_ms, id";
    pub const OLDEST: &str = "SELECT MIN(at_ms) FROM af_activity";
    pub const DAILY_DELETE: &str = "DELETE FROM af_activity_daily WHERE day = $1";
    pub const DAILY_INSERT: &str = "INSERT INTO af_activity_daily (day, player, row_data) VALUES ($1, $2, $3)";
    pub const DAILY_ALL: &str =
        "SELECT day, player, row_data FROM af_activity_daily WHERE day >= $1 AND day <= $2 ORDER BY day, player LIMIT $3";
    pub const DAILY_ONE: &str =
        "SELECT day, player, row_data FROM af_activity_daily WHERE day >= $1 AND day <= $2 AND player = $3 ORDER BY day, player LIMIT $4";
    pub const DAILY_DAYS: &str = "SELECT DISTINCT day FROM af_activity_daily ORDER BY day";
}

fn activity_rows(rows: &[postgres::Row]) -> Vec<ActivityRow> {
    rows.iter()
        .map(|r| ActivityRow {
            at_ms: r.get::<_, i64>(0) as u64,
            player: r.get::<_, Option<i64>>(1).map(|p| p as u64),
            data: r.get(2),
        })
        .collect()
}

fn daily_rows(rows: &[postgres::Row]) -> Vec<DailyRow> {
    rows.iter()
        .map(|r| DailyRow {
            day: r.get(0),
            player: r.get::<_, i64>(1) as u64,
            data: r.get(2),
        })
        .collect()
}

fn daily_replace_in(c: &mut Client, day: &str, rows: &[DailyRow]) -> std::result::Result<(), postgres::Error> {
    let mut tx = c.transaction()?;
    tx.execute(sql::DAILY_DELETE, &[&day])?;
    let ins = tx.prepare(sql::DAILY_INSERT)?;
    for r in rows.iter().filter(|r| r.day == day) {
        tx.execute(&ins, &[&r.day, &(r.player as i64), &r.data])?;
    }
    tx.commit()
}

fn daily_query_in(
    c: &mut Client,
    from: &str,
    to: &str,
    player: Option<u64>,
    limit: usize,
) -> std::result::Result<Vec<postgres::Row>, postgres::Error> {
    let lim = limit.min(i64::MAX as usize) as i64;
    match player {
        Some(p) => c.query(sql::DAILY_ONE, &[&from, &to, &(p as i64), &lim]),
        None => c.query(sql::DAILY_ALL, &[&from, &to, &lim]),
    }
}

/// The advisory lock key for the universe in the current schema.
const LOCK_KEY: &str = "hashtext('mcpwars:' || current_schema())::bigint";

impl From<postgres::Error> for StoreError {
    fn from(e: postgres::Error) -> Self {
        // `Display` hides the server's message; include it.
        match e.as_db_error() {
            Some(db) => StoreError::Db(format!("postgres: {}: {}", db.code().code(), db.message())),
            None => StoreError::Db(format!("postgres: {e}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Take the writer lock (refused with `Busy` if another process holds it).
    Writer,
    /// Read only; writes are refused.
    Reader,
    /// Read only, inside one repeatable-read transaction: a consistent export
    /// while the writer keeps running.
    Snapshot,
}

pub struct PgBackend {
    client: Mutex<Client>,
    access: Access,
    url: String,
    describe: String,
}

/// True for a Postgres connection string.
pub fn is_postgres(loc: &str) -> bool {
    loc.starts_with("postgres://") || loc.starts_with("postgresql://")
}

fn io_err(e: impl std::fmt::Display) -> StoreError {
    StoreError::Db(format!("postgres: {e}"))
}

/// Connect with keepalives (a dead peer is noticed in about a minute) and TLS
/// unless `sslmode=disable`. With the default `prefer`, a failed TLS attempt
/// falls back to plain (private networks often have no certificate for the
/// internal host name).
pub fn connect(url: &str) -> Result<Client> {
    let mut cfg: Config = url.parse().map_err(io_err)?;
    cfg.application_name("mcpwars")
        .connect_timeout(Duration::from_secs(10))
        .keepalives(true)
        .keepalives_idle(Duration::from_secs(20))
        .keepalives_interval(Duration::from_secs(10))
        .keepalives_retries(3);
    if cfg.get_ssl_mode() == SslMode::Disable {
        return Ok(cfg.connect(NoTls)?);
    }
    let tls = postgres_native_tls::MakeTlsConnector::new(native_tls::TlsConnector::new().map_err(io_err)?);
    match cfg.connect(tls) {
        Ok(c) => Ok(c),
        Err(e) if cfg.get_ssl_mode() == SslMode::Prefer => {
            tracing_fallback(&e);
            Ok(cfg.connect(NoTls)?)
        }
        Err(e) => Err(e.into()),
    }
}

/// Say once per process that TLS was unavailable (private networks often have none).
fn tracing_fallback(e: &postgres::Error) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!("postgres: TLS unavailable ({e}); using a plain connection (sslmode=prefer)");
    }
}

/// `postgres://user@host:port/db`, without the password.
fn redact(url: &str) -> String {
    match url.parse::<Config>() {
        Ok(c) => {
            let host = c
                .get_hosts()
                .first()
                .map(|h| match h {
                    postgres::config::Host::Tcp(s) => s.clone(),
                    #[cfg(unix)]
                    postgres::config::Host::Unix(p) => p.display().to_string(),
                })
                .unwrap_or_default();
            let port = c.get_ports().first().map(|p| format!(":{p}")).unwrap_or_default();
            format!(
                "postgres://{}@{host}{port}/{}",
                c.get_user().unwrap_or(""),
                c.get_dbname().unwrap_or("")
            )
        }
        Err(_) => "postgres://(unparseable)".into(),
    }
}

fn seq_i64(seq: u64) -> i64 {
    seq as i64
}

impl PgBackend {
    pub fn open(url: &str, access: Access) -> Result<Self> {
        let mut client = connect(url)?;
        if access == Access::Writer {
            let got: bool = client.query_one(&format!("SELECT pg_try_advisory_lock({LOCK_KEY})"), &[])?.get(0);
            if !got {
                return Err(StoreError::Busy(redact(url)));
            }
        }
        client.batch_execute(SCHEMA_SQL)?;
        if access == Access::Snapshot {
            client.batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")?;
        }
        Ok(PgBackend {
            client: Mutex::new(client),
            access,
            url: url.to_string(),
            describe: redact(url),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    fn client(&self) -> std::sync::MutexGuard<'_, Client> {
        self.client.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn writable(&self) -> Result<()> {
        if self.access == Access::Writer {
            Ok(())
        } else {
            Err(StoreError::Db(
                "this connection is read-only (it does not hold the writer lock)".into(),
            ))
        }
    }
}

/// Ask the current writer of the universe at `url` to hand over.
pub fn request_handoff(url: &str) -> Result<()> {
    let mut c = connect(url)?;
    c.execute("SELECT pg_notify($1, current_schema())", &[&HANDOFF_CHANNEL])?;
    Ok(())
}

/// Whether some server currently holds the writer lock of the universe at `url`.
pub fn writer_present(url: &str) -> Result<bool> {
    let mut c = connect(url)?;
    let got: bool = c.query_one(&format!("SELECT pg_try_advisory_lock({LOCK_KEY})"), &[])?.get(0);
    if got {
        c.execute(&format!("SELECT pg_advisory_unlock({LOCK_KEY})"), &[])?;
    }
    Ok(!got)
}

/// Block until a successor asks for a handoff (true), or the listening
/// connection fails (false).
pub fn wait_for_handoff(url: &str) -> Result<bool> {
    let mut c = connect(url)?;
    let schema: String = c.query_one("SELECT current_schema()", &[])?.get(0);
    c.batch_execute(&format!("LISTEN {HANDOFF_CHANNEL}"))?;
    let mut n = c.notifications();
    let mut it = n.blocking_iter();
    loop {
        match postgres::fallible_iterator::FallibleIterator::next(&mut it) {
            Ok(Some(note)) if note.payload() == schema => return Ok(true),
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return Ok(false),
        }
    }
}

impl Backend for PgBackend {
    fn describe(&self) -> String {
        self.describe.clone()
    }

    fn meta_get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let row = self.client().query_opt("SELECT value FROM af_meta WHERE key = $1", &[&key])?;
        Ok(row.map(|r| r.get(0)))
    }

    fn meta_put(&self, pairs: &[(&str, &[u8])]) -> Result<()> {
        self.writable()?;
        let mut c = self.client();
        let mut tx = c.transaction()?;
        for (k, v) in pairs {
            tx.execute(
                "INSERT INTO af_meta (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value",
                &[k, v],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn append(&self, seq: u64, entry: &[u8], checkpoint: Option<&str>, private: &[crate::private_archive::ArchiveRow]) -> Result<()> {
        self.writable()?;
        let mut c = self.client();
        let mut tx = c.transaction()?;
        let last: i64 = tx.query_one("SELECT COALESCE(MAX(seq), 0) FROM af_journal", &[])?.get(0);
        if seq_i64(seq) != last + 1 {
            return Err(StoreError::OutOfOrder {
                expected: last as u64 + 1,
                got: seq,
            });
        }
        tx.execute("INSERT INTO af_journal (seq, entry) VALUES ($1, $2)", &[&seq_i64(seq), &entry])?;
        if let Some(h) = checkpoint {
            tx.execute("INSERT INTO af_checkpoints (seq, hash) VALUES ($1, $2)", &[&seq_i64(seq), &h])?;
        }
        private_insert(&mut tx, private)?;
        tx.commit()?;
        Ok(())
    }

    fn private_insert(&self, rows: &[crate::private_archive::ArchiveRow]) -> Result<()> {
        self.writable()?;
        let mut c = self.client();
        let mut tx = c.transaction()?;
        private_insert(&mut tx, rows)?;
        tx.commit()?;
        Ok(())
    }
    fn private_query(&self, q: &crate::private_archive::ArchiveQuery) -> Result<Vec<crate::private_archive::ArchiveRow>> {
        let player = q.player.map(seq_i64);
        let seq = seq_i64(q.after_seq);
        let ordinal = seq_i64(q.after_ordinal);
        let limit = q.limit.min(i64::MAX as usize) as i64;
        let mut sql =
            String::from("SELECT seq, ordinal, player, kind, key, row_data FROM af_private_archive WHERE (seq,ordinal) > ($1,$2)");
        let mut params: Vec<&(dyn postgres::types::ToSql + Sync)> = vec![&seq, &ordinal];
        if let Some(p) = &player {
            params.push(p);
            sql.push_str(&format!(" AND player = ${}", params.len()));
        }
        if let Some(k) = &q.kind {
            params.push(k);
            sql.push_str(&format!(" AND kind = ${}", params.len()));
        }
        if let Some(k) = &q.key {
            params.push(k);
            sql.push_str(&format!(" AND key = ${}", params.len()));
        }
        params.push(&limit);
        sql.push_str(&format!(" ORDER BY seq,ordinal LIMIT ${}", params.len()));
        Ok(self
            .client()
            .query(&sql, &params)?
            .into_iter()
            .map(|r| crate::private_archive::ArchiveRow {
                seq: r.get::<_, i64>(0) as u64,
                ordinal: r.get::<_, i64>(1) as u64,
                player: r.get::<_, i64>(2) as u64,
                kind: r.get(3),
                key: r.get(4),
                data: r.get(5),
            })
            .collect())
    }

    fn last_seq(&self) -> Result<u64> {
        let last: i64 = self.client().query_one("SELECT COALESCE(MAX(seq), 0) FROM af_journal", &[])?.get(0);
        Ok(last as u64)
    }

    fn journal_len(&self) -> Result<u64> {
        let n: i64 = self.client().query_one("SELECT COUNT(*) FROM af_journal", &[])?.get(0);
        Ok(n as u64)
    }

    fn entries_page(&self, after: u64, limit: usize) -> Result<Vec<(u64, Vec<u8>)>> {
        let rows = self.client().query(
            "SELECT seq, entry FROM af_journal WHERE seq > $1 ORDER BY seq LIMIT $2",
            &[&seq_i64(after), &(limit as i64)],
        )?;
        Ok(rows.iter().map(|r| (r.get::<_, i64>(0) as u64, r.get(1))).collect())
    }

    fn checkpoints_after(&self, after: u64) -> Result<Vec<(u64, String)>> {
        let rows = self.client().query(
            "SELECT seq, hash FROM af_checkpoints WHERE seq > $1 ORDER BY seq",
            &[&seq_i64(after)],
        )?;
        Ok(rows.iter().map(|r| (r.get::<_, i64>(0) as u64, r.get(1))).collect())
    }

    fn snapshot_put(&self, seq: u64, row: &[u8], keep: usize) -> Result<()> {
        self.writable()?;
        let mut c = self.client();
        let mut tx = c.transaction()?;
        tx.execute(
            "INSERT INTO af_snapshots (seq, row_data) VALUES ($1, $2) ON CONFLICT (seq) DO UPDATE SET row_data = EXCLUDED.row_data",
            &[&seq_i64(seq), &row],
        )?;
        tx.execute(
            "DELETE FROM af_snapshots WHERE seq NOT IN (SELECT seq FROM af_snapshots ORDER BY seq DESC LIMIT $1)",
            &[&(keep as i64)],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn snapshot_last(&self) -> Result<Option<(u64, Vec<u8>)>> {
        let row = self
            .client()
            .query_opt("SELECT seq, row_data FROM af_snapshots ORDER BY seq DESC LIMIT 1", &[])?;
        Ok(row.map(|r| (r.get::<_, i64>(0) as u64, r.get(1))))
    }

    fn snapshot_last_seq(&self) -> Result<Option<u64>> {
        let row = self.client().query_opt("SELECT MAX(seq) FROM af_snapshots", &[])?;
        Ok(row.and_then(|r| r.get::<_, Option<i64>>(0)).map(|s| s as u64))
    }

    fn bulk_load(&self, entries: &[(u64, Vec<u8>)], checkpoints: &[(u64, String)]) -> Result<()> {
        self.writable()?;
        let mut c = self.client();
        let mut tx = c.transaction()?;
        let ins = tx.prepare("INSERT INTO af_journal (seq, entry) VALUES ($1, $2)")?;
        for (k, v) in entries {
            tx.execute(&ins, &[&seq_i64(*k), v])?;
        }
        let ins = tx.prepare("INSERT INTO af_checkpoints (seq, hash) VALUES ($1, $2)")?;
        for (k, h) in checkpoints {
            tx.execute(&ins, &[&seq_i64(*k), h])?;
        }
        tx.commit()?;
        Ok(())
    }

    fn activity_append(&self, rows: &[ActivityRow]) -> Result<()> {
        self.writable()?;
        let mut c = self.client();
        let mut tx = c.transaction()?;
        let ins = tx.prepare("INSERT INTO af_activity (at_ms, player, row_data) VALUES ($1, $2, $3)")?;
        for r in rows {
            tx.execute(&ins, &[&(r.at_ms as i64), &r.player.map(|p| p as i64), &r.data])?;
        }
        tx.commit()?;
        Ok(())
    }

    fn activity_query(&self, player: Option<u64>, since_ms: u64, limit: usize) -> Result<Vec<ActivityRow>> {
        let since = since_ms as i64;
        let lim = limit as i64;
        let rows = match player {
            Some(p) => self.client().query(
                "SELECT at_ms, player, row_data FROM af_activity WHERE player = $1 AND at_ms >= $2 ORDER BY at_ms DESC, id DESC LIMIT $3",
                &[&(p as i64), &since, &lim],
            )?,
            None => self.client().query(
                "SELECT at_ms, player, row_data FROM af_activity WHERE at_ms >= $1 ORDER BY at_ms DESC, id DESC LIMIT $2",
                &[&since, &lim],
            )?,
        };
        Ok(rows
            .iter()
            .map(|r| ActivityRow {
                at_ms: r.get::<_, i64>(0) as u64,
                player: r.get::<_, Option<i64>>(1).map(|p| p as u64),
                data: r.get(2),
            })
            .collect())
    }

    fn activity_prune(&self, before_ms: u64) -> Result<u64> {
        self.writable()?;
        Ok(self
            .client()
            .execute("DELETE FROM af_activity WHERE at_ms < $1", &[&(before_ms as i64)])?)
    }

    fn activity_between(&self, from_ms: u64, to_ms: u64) -> Result<Vec<ActivityRow>> {
        let rows = self.client().query(sql::BETWEEN, &[&(from_ms as i64), &(to_ms as i64)])?;
        Ok(activity_rows(&rows))
    }

    fn activity_oldest(&self) -> Result<Option<u64>> {
        let row = self.client().query_one(sql::OLDEST, &[])?;
        Ok(row.get::<_, Option<i64>>(0).map(|v| v as u64))
    }

    fn daily_replace(&self, day: &str, rows: &[DailyRow]) -> Result<()> {
        self.writable()?;
        Ok(daily_replace_in(&mut self.client(), day, rows)?)
    }

    fn daily_query(&self, from: &str, to: &str, player: Option<u64>, limit: usize) -> Result<Vec<DailyRow>> {
        let rows = daily_query_in(&mut self.client(), from, to, player, limit)?;
        Ok(daily_rows(&rows))
    }

    fn daily_days(&self) -> Result<Vec<String>> {
        let rows = self.client().query(sql::DAILY_DAYS, &[])?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    fn finish(&self) -> Result<()> {
        if self.access == Access::Snapshot {
            self.client().batch_execute("COMMIT")?;
        }
        Ok(())
    }

    fn debug_put(&self, table: Table, key: u64, value: &[u8]) -> Result<()> {
        let sql = match table {
            Table::Journal => "UPDATE af_journal SET entry = $2 WHERE seq = $1",
            Table::Snapshots => "UPDATE af_snapshots SET row_data = $2 WHERE seq = $1",
        };
        self.client().execute(sql, &[&seq_i64(key), &value])?;
        Ok(())
    }

    fn debug_get(&self, table: Table, key: u64) -> Result<Option<Vec<u8>>> {
        let sql = match table {
            Table::Journal => "SELECT entry FROM af_journal WHERE seq = $1",
            Table::Snapshots => "SELECT row_data FROM af_snapshots WHERE seq = $1",
        };
        Ok(self.client().query_opt(sql, &[&seq_i64(key)])?.map(|r| r.get(0)))
    }
}

/// The interaction log on its own Postgres connection, for a worker thread beside the engine:
/// telemetry writes and reads never wait on, or delay, the journal. Outside the writer lock by
/// design (a standby server may log the requests it refuses). Reconnects on the next call after
/// a failure.
pub struct ActivityLog {
    url: String,
    client: Option<Client>,
}

impl ActivityLog {
    pub fn new(url: &str) -> Self {
        ActivityLog {
            url: url.to_string(),
            client: None,
        }
    }

    fn with<T>(&mut self, f: impl FnOnce(&mut Client) -> std::result::Result<T, postgres::Error>) -> Result<T> {
        if self.client.as_ref().is_none_or(|c| c.is_closed()) {
            self.client = Some(connect(&self.url)?);
        }
        let c = self.client.as_mut().expect("connected above");
        match f(c) {
            Ok(v) => Ok(v),
            Err(e) => {
                self.client = None;
                Err(io_err(e))
            }
        }
    }

    /// Append rows in one statement.
    pub fn append(&mut self, rows: &[ActivityRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let at: Vec<i64> = rows.iter().map(|r| r.at_ms as i64).collect();
        let player: Vec<Option<i64>> = rows.iter().map(|r| r.player.map(|p| p as i64)).collect();
        let data: Vec<&[u8]> = rows.iter().map(|r| r.data.as_slice()).collect();
        self.with(|c| {
            c.execute(
                "INSERT INTO af_activity (at_ms, player, row_data) \
                 SELECT * FROM unnest($1::bigint[], $2::bigint[], $3::bytea[])",
                &[&at, &player, &data],
            )
        })?;
        Ok(())
    }

    /// Rows at or after `since_ms`, newest first, for one player or all, at most `limit`.
    pub fn query(&mut self, player: Option<u64>, since_ms: u64, limit: usize) -> Result<Vec<ActivityRow>> {
        let (since, lim) = (since_ms as i64, limit as i64);
        let rows = self.with(|c| match player {
            Some(p) => c.query(
                "SELECT at_ms, player, row_data FROM af_activity WHERE player = $1 AND at_ms >= $2 ORDER BY at_ms DESC, id DESC LIMIT $3",
                &[&(p as i64), &since, &lim],
            ),
            None => c.query(
                "SELECT at_ms, player, row_data FROM af_activity WHERE at_ms >= $1 ORDER BY at_ms DESC, id DESC LIMIT $2",
                &[&since, &lim],
            ),
        })?;
        Ok(rows
            .iter()
            .map(|r| ActivityRow {
                at_ms: r.get::<_, i64>(0) as u64,
                player: r.get::<_, Option<i64>>(1).map(|p| p as u64),
                data: r.get(2),
            })
            .collect())
    }

    /// Delete rows older than `before_ms`; returns how many.
    pub fn prune(&mut self, before_ms: u64) -> Result<u64> {
        self.with(|c| c.execute("DELETE FROM af_activity WHERE at_ms < $1", &[&(before_ms as i64)]))
    }

    /// Rows with `from_ms <= at_ms < to_ms`, oldest first.
    pub fn between(&mut self, from_ms: u64, to_ms: u64) -> Result<Vec<ActivityRow>> {
        let rows = self.with(|c| c.query(sql::BETWEEN, &[&(from_ms as i64), &(to_ms as i64)]))?;
        Ok(activity_rows(&rows))
    }

    /// The time of the oldest row, if any.
    pub fn oldest(&mut self) -> Result<Option<u64>> {
        let row = self.with(|c| c.query_one(sql::OLDEST, &[]))?;
        Ok(row.get::<_, Option<i64>>(0).map(|v| v as u64))
    }

    /// Replace one day's daily summary rows (see `Backend::daily_replace`).
    pub fn daily_replace(&mut self, day: &str, rows: &[DailyRow]) -> Result<()> {
        self.with(|c| daily_replace_in(c, day, rows))
    }

    /// Daily summary rows (see `Backend::daily_query`).
    pub fn daily_query(&mut self, from: &str, to: &str, player: Option<u64>, limit: usize) -> Result<Vec<DailyRow>> {
        let rows = self.with(|c| daily_query_in(c, from, to, player, limit))?;
        Ok(daily_rows(&rows))
    }

    /// Every day with daily summary rows, oldest first.
    pub fn daily_days(&mut self) -> Result<Vec<String>> {
        let rows = self.with(|c| c.query(sql::DAILY_DAYS, &[]))?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }
}
