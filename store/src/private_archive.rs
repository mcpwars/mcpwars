//! Permanent operator-only history of private working data. Never part of World or telemetry.
use std::collections::BTreeMap;

use af_core::cmd::{AdminArgs, NoteMode};
use af_core::engine::{Actor, Entry};
use af_core::memory::{Action, Record, Version};
use af_core::{Command, World};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveRow {
    pub seq: u64,
    pub ordinal: u64,
    pub player: u64,
    pub kind: String,
    pub key: String,
    /// Complete JSON event, including its original body and provenance.
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ArchiveQuery {
    pub player: Option<u64>,
    pub kind: Option<String>,
    pub key: Option<String>,
    /// Ascending keyset cursor. Always follow the returned complete cursor.
    pub after_seq: u64,
    pub after_ordinal: u64,
    pub limit: usize,
}

#[derive(Clone, Default)]
struct PrivatePlayer {
    name: String,
    notes: BTreeMap<String, String>,
    memory: BTreeMap<String, Record>,
}

#[derive(Clone, Default)]
pub struct Capture {
    players: BTreeMap<u64, PrivatePlayer>,
    tick: u64,
    epoch: Option<u32>,
    season: Option<u32>,
}

impl Capture {
    /// Capture only commands that can change private working data.
    pub fn before(w: &World, e: &Entry) -> Self {
        let target = match (&e.cmd, e.actor) {
            (Command::Note(_) | Command::Memory(_), Actor::Player(pid)) => Some(pid),
            (Command::Admin(AdminArgs::ClearNotes { player } | AdminArgs::RemovePlayer { player }), _) => Some(*player),
            (Command::Tick | Command::Admin(AdminArgs::FreshStart), _) => None,
            _ => return Self::default(),
        };
        let mut c = Self {
            tick: w.tick,
            epoch: Some(w.archive.epoch),
            season: Some(w.season.number),
            ..Self::default()
        };
        for p in w.players.values().filter(|p| p.npc.is_none() && target.is_none_or(|id| id == p.id)) {
            if matches!(e.cmd, Command::Tick)
                && !p.memory.values().any(|r| {
                    r.versions
                        .last()
                        .is_some_and(|v| v.expires_tick.is_some_and(|t| t <= w.tick.saturating_add(1)))
                })
            {
                continue;
            }
            c.players.insert(
                u64::from(p.id),
                PrivatePlayer {
                    name: p.name.clone(),
                    notes: p.notes.clone(),
                    memory: p.memory.clone(),
                },
            );
        }
        c
    }

    pub fn after(&self, w: &World, e: &Entry) -> crate::Result<Vec<ArchiveRow>> {
        let mut next = Self {
            tick: w.tick,
            epoch: Some(w.archive.epoch),
            season: Some(w.season.number),
            ..Self::default()
        };
        for id in self.players.keys() {
            if let Some(p) = w.players.get(&(*id as u32)) {
                next.players.insert(
                    *id,
                    PrivatePlayer {
                        name: p.name.clone(),
                        notes: p.notes.clone(),
                        memory: p.memory.clone(),
                    },
                );
            }
        }
        self.diff(&next, e, false)
    }

    fn diff(&self, next: &Self, e: &Entry, legacy: bool) -> crate::Result<Vec<ArchiveRow>> {
        let mut rows = vec![];
        let reason = match &e.cmd {
            Command::Tick => "expired",
            Command::Admin(AdminArgs::FreshStart) => "fresh_start",
            Command::Admin(AdminArgs::RemovePlayer { .. }) => "captain_removed",
            Command::Admin(AdminArgs::ClearNotes { .. }) => "operator_clear",
            _ => "archived",
        };
        let mut add = |pid: u64, p: &PrivatePlayer, kind: &str, key: &str, reason: &str, body: Value| -> crate::Result<()> {
            let data = json!({"seq":e.seq,"ordinal":rows.len(),"player":pid,"name":p.name,"kind":kind,"key":key,"tick":next.tick,"at_ms":e.at_ms,"epoch":self.epoch,"season":self.season,"reason":reason,"legacy":legacy,"body":body});
            rows.push(ArchiveRow {
                seq: e.seq,
                ordinal: rows.len() as u64,
                player: pid,
                kind: kind.into(),
                key: key.into(),
                data: serde_json::to_vec(&data)?,
            });
            Ok(())
        };
        for (&pid, p) in &self.players {
            let empty = PrivatePlayer::default();
            let n = next.players.get(&pid).unwrap_or(&empty);
            for (k, v) in &p.notes {
                if n.notes.get(k) != Some(v) {
                    add(
                        pid,
                        p,
                        "note",
                        k,
                        if n.notes.contains_key(k) { "replaced" } else { reason },
                        json!({"text":v}),
                    )?;
                }
            }
            for (k, v) in &n.notes {
                let forced = matches!((&e.cmd,e.actor),(Command::Note(a),Actor::Player(id)) if u64::from(id)==pid && af_core::player::sanitize(&a.key,40)==*k);
                if p.notes.get(k) != Some(v) || forced {
                    add(pid, n, "note", k, "saved", json!({"text":v}))?;
                }
            }
            for (k, r) in &p.memory {
                let replaced = n
                    .memory
                    .get(k)
                    .is_some_and(|v| v.versions.first().map(|v| v.revision) != r.versions.first().map(|v| v.revision));
                if !n.memory.contains_key(k) || replaced {
                    add(
                        pid,
                        p,
                        "memory",
                        k,
                        if replaced { "expired" } else { reason },
                        json!({"versions":r.versions}),
                    )?;
                }
            }
            for (k, r) in &n.memory {
                let last = p.memory.get(k).and_then(|r| r.versions.last()).map_or(0, |v| v.revision);
                for v in r.versions.iter().filter(|v| v.revision > last) {
                    add(pid, n, "memory", k, "saved", json!({"version":v}))?;
                }
            }
        }
        Ok(rows)
    }

    /// Reconstruct private writes from old journals without replaying obsolete game semantics.
    /// Only accepted commands occur here. Original commands remain the ultimate source of truth.
    pub fn legacy_step(&mut self, e: &Entry) -> crate::Result<Vec<ArchiveRow>> {
        if matches!(e.cmd, Command::Tick) {
            self.tick += 1;
            return Ok(vec![]);
        }
        let before = self.clone();
        match (&e.cmd, e.actor) {
            (Command::Tick, _) => self.tick += 1,
            (Command::Note(a), Actor::Player(id)) => {
                let p = self.players.entry(u64::from(id)).or_default();
                let k = af_core::player::sanitize(&a.key, 40);
                match a.mode.unwrap_or(NoteMode::Replace) {
                    NoteMode::Delete | NoteMode::Archive => {
                        p.notes.remove(&k);
                    }
                    NoteMode::Replace => {
                        p.notes.insert(k, a.value.clone());
                    }
                    NoteMode::Append => {
                        let text = p.notes.entry(k).or_default();
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&a.value);
                    }
                }
            }
            (Command::Memory(a), Actor::Player(id)) => {
                let p = self.players.entry(u64::from(id)).or_default();
                if a.action == Action::Import {
                    let bundle = a
                        .data
                        .as_ref()
                        .ok_or_else(|| crate::StoreError::Db("memory import without body in journal".into()))?;
                    p.notes
                        .extend(serde_json::from_value::<BTreeMap<String, String>>(bundle["notes"].clone())?);
                    p.memory
                        .extend(serde_json::from_value::<BTreeMap<String, Record>>(bundle["records"].clone())?);
                } else if let Some(k) = &a.key {
                    if a.action == Action::Archive {
                        p.memory.remove(k);
                    } else {
                        let r = p.memory.entry(k.clone()).or_default();
                        let old = r.versions.last();
                        let mut data = a.data.clone().unwrap_or(Value::Null);
                        if a.action == Action::Patch {
                            let mut v = old.map(|v| v.data.clone()).unwrap_or(Value::Null);
                            merge(&mut v, &data);
                            data = v;
                        }
                        if a.action == Action::Append {
                            let mut v = old.map(|v| v.data.clone()).unwrap_or(json!([]));
                            if let Some(v) = v.as_array_mut() {
                                v.push(data);
                            }
                            data = v;
                        }
                        r.versions.push(Version {
                            revision: old.map_or(1, |v| v.revision + 1),
                            tick: self.tick,
                            expires_tick: a
                                .ttl_ticks
                                .map(|t| self.tick.saturating_add(t))
                                .or_else(|| old.and_then(|v| v.expires_tick)),
                            data,
                        });
                    }
                }
            }
            (Command::Admin(AdminArgs::ClearNotes { player }), _) => {
                if let Some(p) = self.players.get_mut(&u64::from(*player)) {
                    p.notes.clear();
                }
            }
            (Command::Admin(AdminArgs::RemovePlayer { player }), _) => {
                self.players.remove(&u64::from(*player));
            }
            (Command::Admin(AdminArgs::FreshStart), _) => {
                for p in self.players.values_mut() {
                    p.notes.clear();
                    p.memory.clear();
                }
            }
            _ => {}
        }
        // Include a newly encountered captain in the before map so its first save is indexed.
        let mut before = before;
        for (&id, p) in &self.players {
            before.players.entry(id).or_insert_with(|| PrivatePlayer {
                name: p.name.clone(),
                ..PrivatePlayer::default()
            });
        }
        before.diff(self, e, true)
    }
}

fn merge(target: &mut Value, patch: &Value) {
    if let Value::Object(p) = patch {
        if !target.is_object() {
            *target = json!({});
        }
        let t = target.as_object_mut().unwrap();
        for (k, v) in p {
            if v.is_null() {
                t.remove(k);
            } else {
                merge(t.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
    } else {
        *target = patch.clone();
    }
}
