//! Embedded single-file backend (local play, development, portable backups).
//! redb holds an exclusive file lock, so one process writes a file at a time.

use std::path::{Path, PathBuf};

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};

use crate::backend::{ActivityRow, Backend, DailyRow, Table};
use crate::{Result, StoreError};

const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const JOURNAL: TableDefinition<u64, &[u8]> = TableDefinition::new("journal");
const CHECKPOINTS: TableDefinition<u64, &[u8]> = TableDefinition::new("checkpoints");
const SNAPSHOTS: TableDefinition<u64, &[u8]> = TableDefinition::new("snapshots");
/// Interaction log: (at_ms, n) -> player (8 bytes, u64::MAX for none) + row. `n` only grows.
const ACTIVITY: TableDefinition<(u64, u64), &[u8]> = TableDefinition::new("activity");
const PRIVATE: TableDefinition<(u64, u64), &[u8]> = TableDefinition::new("private_archive");
const PRIVATE_PLAYER: TableDefinition<(u64, u64, u64), &[u8]> = TableDefinition::new("private_archive_player");
const NO_PLAYER: u64 = u64::MAX;
/// Daily activity summaries, kept forever: (day "YYYY-MM-DD", player) -> summary JSON.
const ACTIVITY_DAILY: TableDefinition<(&str, u64), &[u8]> = TableDefinition::new("activity_daily");

macro_rules! db_err {
    ($($t:ty),*) => { $(impl From<$t> for StoreError { fn from(e: $t) -> Self { StoreError::Db(e.to_string()) } })* };
}
db_err!(
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError
);

pub struct RedbBackend {
    db: Database,
    path: PathBuf,
}

impl RedbBackend {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Database::create(path)?;
        let tx = db.begin_write()?;
        tx.open_table(META)?;
        tx.open_table(JOURNAL)?;
        tx.open_table(CHECKPOINTS)?;
        tx.open_table(SNAPSHOTS)?;
        tx.open_table(ACTIVITY)?;
        tx.open_table(ACTIVITY_DAILY)?;
        tx.open_table(PRIVATE)?;
        tx.open_table(PRIVATE_PLAYER)?;
        tx.commit()?;
        Ok(RedbBackend {
            db,
            path: path.to_path_buf(),
        })
    }

    fn def(t: Table) -> TableDefinition<'static, u64, &'static [u8]> {
        match t {
            Table::Journal => JOURNAL,
            Table::Snapshots => SNAPSHOTS,
        }
    }
}

fn private_insert(tx: &redb::WriteTransaction, rows: &[crate::private_archive::ArchiveRow]) -> Result<()> {
    let mut table = tx.open_table(PRIVATE)?;
    let mut players = tx.open_table(PRIVATE_PLAYER)?;
    for r in rows {
        if table.get((r.seq, r.ordinal))?.is_none() {
            let data = serde_json::to_vec(r)?;
            table.insert((r.seq, r.ordinal), data.as_slice())?;
            players.insert((r.player, r.seq, r.ordinal), data.as_slice())?;
        }
    }
    Ok(())
}

impl Backend for RedbBackend {
    fn describe(&self) -> String {
        self.path.display().to_string()
    }

    fn meta_get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(META)?;
        Ok(t.get(key)?.map(|v| v.value().to_vec()))
    }

    fn meta_put(&self, pairs: &[(&str, &[u8])]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(META)?;
            for (k, v) in pairs {
                t.insert(*k, *v)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn append(&self, seq: u64, entry: &[u8], checkpoint: Option<&str>, private: &[crate::private_archive::ArchiveRow]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(JOURNAL)?;
            let last = t.last()?.map(|(k, _)| k.value()).unwrap_or(0);
            if seq != last + 1 {
                return Err(StoreError::OutOfOrder {
                    expected: last + 1,
                    got: seq,
                });
            }
            t.insert(seq, entry)?;
            if let Some(h) = checkpoint {
                tx.open_table(CHECKPOINTS)?.insert(seq, h.as_bytes())?;
            }
        }
        private_insert(&tx, private)?;
        tx.commit()?;
        Ok(())
    }

    fn private_insert(&self, rows: &[crate::private_archive::ArchiveRow]) -> Result<()> {
        let tx = self.db.begin_write()?;
        private_insert(&tx, rows)?;
        tx.commit()?;
        Ok(())
    }
    fn private_query(&self, q: &crate::private_archive::ArchiveQuery) -> Result<Vec<crate::private_archive::ArchiveRow>> {
        if q.limit == 0 {
            return Ok(vec![]);
        }
        let tx = self.db.begin_read()?;
        let mut rows = vec![];
        let mut add = |data: &[u8]| -> Result<bool> {
            let r: crate::private_archive::ArchiveRow = serde_json::from_slice(data)?;
            if q.kind.as_ref().is_none_or(|k| *k == r.kind) && q.key.as_ref().is_none_or(|k| *k == r.key) {
                rows.push(r);
            }
            Ok(rows.len() >= q.limit)
        };
        if let Some(pid) = q.player {
            let table = tx.open_table(PRIVATE_PLAYER)?;
            for row in table.range((
                std::ops::Bound::Excluded((pid, q.after_seq, q.after_ordinal)),
                std::ops::Bound::Included((pid, u64::MAX, u64::MAX)),
            ))? {
                let (_, v) = row?;
                if add(v.value())? {
                    break;
                }
            }
        } else {
            let table = tx.open_table(PRIVATE)?;
            for row in table.range((
                std::ops::Bound::Excluded((q.after_seq, q.after_ordinal)),
                std::ops::Bound::Unbounded,
            ))? {
                let (_, v) = row?;
                if add(v.value())? {
                    break;
                }
            }
        }
        Ok(rows)
    }

    fn last_seq(&self) -> Result<u64> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(JOURNAL)?;
        Ok(t.last()?.map(|(k, _)| k.value()).unwrap_or(0))
    }

    fn journal_len(&self) -> Result<u64> {
        let tx = self.db.begin_read()?;
        Ok(tx.open_table(JOURNAL)?.len()?)
    }

    fn entries_page(&self, after: u64, limit: usize) -> Result<Vec<(u64, Vec<u8>)>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(JOURNAL)?;
        let mut out = vec![];
        for row in t.range(after + 1..)?.take(limit) {
            let (k, v) = row?;
            out.push((k.value(), v.value().to_vec()));
        }
        Ok(out)
    }

    fn checkpoints_after(&self, after: u64) -> Result<Vec<(u64, String)>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(CHECKPOINTS)?;
        let mut out = vec![];
        for row in t.range(after + 1..)? {
            let (k, v) = row?;
            out.push((k.value(), String::from_utf8_lossy(v.value()).into_owned()));
        }
        Ok(out)
    }

    fn snapshot_put(&self, seq: u64, row: &[u8], keep: usize) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(SNAPSHOTS)?;
            t.insert(seq, row)?;
            let keys: Vec<u64> = t.iter()?.filter_map(|r| r.ok().map(|(k, _)| k.value())).collect();
            if keys.len() > keep {
                for k in &keys[..keys.len() - keep] {
                    t.remove(*k)?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn snapshot_last(&self) -> Result<Option<(u64, Vec<u8>)>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(SNAPSHOTS)?;
        Ok(t.last()?.map(|(k, v)| (k.value(), v.value().to_vec())))
    }

    fn snapshot_last_seq(&self) -> Result<Option<u64>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(SNAPSHOTS)?;
        Ok(t.last()?.map(|(k, _)| k.value()))
    }

    fn bulk_load(&self, entries: &[(u64, Vec<u8>)], checkpoints: &[(u64, String)]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut j = tx.open_table(JOURNAL)?;
            for (k, v) in entries {
                j.insert(*k, v.as_slice())?;
            }
            let mut c = tx.open_table(CHECKPOINTS)?;
            for (k, h) in checkpoints {
                c.insert(*k, h.as_bytes())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn activity_append(&self, rows: &[ActivityRow]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut meta = tx.open_table(META)?;
            let mut n = meta
                .get("activity_n")?
                .and_then(|v| v.value().try_into().ok().map(u64::from_be_bytes))
                .unwrap_or(0);
            let mut t = tx.open_table(ACTIVITY)?;
            for r in rows {
                n += 1;
                let mut v = r.player.unwrap_or(NO_PLAYER).to_be_bytes().to_vec();
                v.extend_from_slice(&r.data);
                t.insert((r.at_ms, n), v.as_slice())?;
            }
            meta.insert("activity_n", n.to_be_bytes().as_slice())?;
        }
        tx.commit()?;
        Ok(())
    }

    fn activity_query(&self, player: Option<u64>, since_ms: u64, limit: usize) -> Result<Vec<ActivityRow>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(ACTIVITY)?;
        let mut out = vec![];
        for row in t.range((since_ms, 0)..)?.rev() {
            let (k, v) = row?;
            let v = v.value();
            if v.len() < 8 {
                continue;
            }
            let pid = u64::from_be_bytes(v[..8].try_into().unwrap_or_default());
            let pid = (pid != NO_PLAYER).then_some(pid);
            if player.is_some() && pid != player {
                continue;
            }
            out.push(ActivityRow {
                at_ms: k.value().0,
                player: pid,
                data: v[8..].to_vec(),
            });
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    fn activity_prune(&self, before_ms: u64) -> Result<u64> {
        let tx = self.db.begin_write()?;
        let n;
        {
            let mut t = tx.open_table(ACTIVITY)?;
            let keys: Vec<(u64, u64)> = t
                .range(..(before_ms, 0))?
                .map(|r| r.map(|(k, _)| k.value()))
                .collect::<std::result::Result<_, _>>()?;
            n = keys.len() as u64;
            for k in keys {
                t.remove(k)?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    fn activity_between(&self, from_ms: u64, to_ms: u64) -> Result<Vec<ActivityRow>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(ACTIVITY)?;
        let mut out = vec![];
        if to_ms <= from_ms {
            return Ok(out);
        }
        for row in t.range((from_ms, 0)..(to_ms, 0))? {
            let (k, v) = row?;
            let v = v.value();
            if v.len() < 8 {
                continue;
            }
            let pid = u64::from_be_bytes(v[..8].try_into().unwrap_or_default());
            out.push(ActivityRow {
                at_ms: k.value().0,
                player: (pid != NO_PLAYER).then_some(pid),
                data: v[8..].to_vec(),
            });
        }
        Ok(out)
    }

    fn activity_oldest(&self) -> Result<Option<u64>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(ACTIVITY)?;
        Ok(t.first()?.map(|(k, _)| k.value().0))
    }

    fn daily_replace(&self, day: &str, rows: &[DailyRow]) -> Result<()> {
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(ACTIVITY_DAILY)?;
            let keys: Vec<u64> = t
                .range((day, 0)..=(day, u64::MAX))?
                .map(|r| r.map(|(k, _)| k.value().1))
                .collect::<std::result::Result<_, _>>()?;
            for p in keys {
                t.remove((day, p))?;
            }
            for r in rows.iter().filter(|r| r.day == day) {
                t.insert((day, r.player), r.data.as_slice())?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn daily_query(&self, from: &str, to: &str, player: Option<u64>, limit: usize) -> Result<Vec<DailyRow>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(ACTIVITY_DAILY)?;
        let mut out = vec![];
        if to < from {
            return Ok(out);
        }
        for row in t.range((from, 0)..=(to, u64::MAX))? {
            let (k, v) = row?;
            let (day, p) = k.value();
            if player.is_some_and(|x| x != p) {
                continue;
            }
            out.push(DailyRow {
                day: day.to_string(),
                player: p,
                data: v.value().to_vec(),
            });
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    fn daily_days(&self) -> Result<Vec<String>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(ACTIVITY_DAILY)?;
        let mut out: Vec<String> = vec![];
        for row in t.iter()? {
            let (k, _) = row?;
            let day = k.value().0;
            if out.last().is_none_or(|d| d != day) {
                out.push(day.to_string());
            }
        }
        Ok(out)
    }

    fn debug_put(&self, table: Table, key: u64, value: &[u8]) -> Result<()> {
        let tx = self.db.begin_write()?;
        tx.open_table(Self::def(table))?.insert(key, value)?;
        tx.commit()?;
        Ok(())
    }

    fn debug_get(&self, table: Table, key: u64) -> Result<Option<Vec<u8>>> {
        let tx = self.db.begin_read()?;
        let t = tx.open_table(Self::def(table))?;
        Ok(t.get(key)?.map(|v| v.value().to_vec()))
    }
}
