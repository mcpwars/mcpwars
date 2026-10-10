//! Private structured memory. Explicit versions and expiry never discard a record.
use crate::engine::{Ctx, Outcome, Refusal, Res};
use crate::model::{PlayerId, Tick, World};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Version {
    pub revision: u64,
    pub tick: Tick,
    pub expires_tick: Option<Tick>,
    pub data: Value,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub versions: Vec<Version>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    List,
    Read,
    Save,
    Patch,
    /// Append one JSON value to an existing array, with a revision check.
    Append,
    /// Changes from expected_revision to the current revision, keyed by JSON pointer.
    Diff,
    History,
    Export,
    Import,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Args {
    pub action: Action,
    #[serde(default)]
    pub key: Option<String>,
    /// JSON value for save, JSON merge patch for patch, or export bundle for import.
    #[serde(default)]
    pub data: Option<Value>,
    /// Optimistic revision check (0 means create only). Required for patch.
    #[serde(default)]
    pub expected_revision: Option<u64>,
    /// Expiry marks a record archived, without deleting its data or history.
    #[serde(default)]
    pub ttl_ticks: Option<Tick>,
    /// JSON pointer for a nested read, e.g. /facts/0/sector.
    #[serde(default)]
    pub path: Option<String>,
    /// list: include expired records (their histories remain available regardless).
    #[serde(default)]
    pub include_archived: bool,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}
impl Args {
    pub fn mutates(&self) -> bool {
        matches!(self.action, Action::Save | Action::Patch | Action::Append | Action::Import)
    }
}
fn key(a: &Args) -> Res<&str> {
    let k = a.key.as_deref().unwrap_or("");
    if k.is_empty() || k.chars().count() > 40 || k == "plan" {
        return Err(Refusal::new(
            "invalid_args",
            "Use a memory key of 1-40 characters other than plan. Keep plan in the private Markdown note.",
        ));
    }
    Ok(k)
}
fn current<'a>(w: &'a World, pid: PlayerId, k: &str) -> Option<&'a Version> {
    w.players.get(&pid)?.memory.get(k)?.versions.last()
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
fn differences(before: &Value, after: &Value, path: &str) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    if before == after {
        return out;
    }
    if let (Some(a), Some(b)) = (before.as_object(), after.as_object()) {
        let keys: std::collections::BTreeSet<_> = a.keys().chain(b.keys()).collect();
        for k in keys {
            let path = format!("{path}/{}", k.replace('~', "~0").replace('/', "~1"));
            if a.contains_key(k) != b.contains_key(k) {
                out.insert(
                    path,
                    json!({"before": a.get(k), "after": b.get(k), "existed_before": a.contains_key(k), "exists_after": b.contains_key(k)}),
                );
            } else {
                out.extend(differences(&a[k], &b[k], &path));
            }
        }
    } else {
        out.insert(path.to_string(), json!({"before": before,"after": after}));
    }
    out
}
pub fn read(w: &World, pid: PlayerId, a: &Args) -> Res<Value> {
    let p = w
        .players
        .get(&pid)
        .ok_or_else(|| Refusal::new("unknown_player", "No such captain."))?;
    match a.action {
        Action::Export => Ok(json!({"format": "mcpwars-private-memory-1", "notes": p.notes, "records": p.memory})),
        Action::List => Ok(
            json!({"records": p.memory.iter().filter(|(_,r)| a.include_archived || r.versions.last().is_some_and(|v| v.expires_tick.is_none_or(|t| t > w.tick))).filter_map(|(k,r)| r.versions.last().map(|v| json!({"key": k, "revision": v.revision, "tick": v.tick, "expires_tick": v.expires_tick, "archived": v.expires_tick.is_some_and(|t| t <= w.tick)}))).collect::<Vec<_>>()}),
        ),
        Action::Diff => {
            let k = key(a)?;
            let r = p.memory.get(k).ok_or_else(|| Refusal::new("not_found", "No such memory record."))?;
            let revision = a
                .expected_revision
                .ok_or_else(|| Refusal::new("invalid_args", "diff needs expected_revision as its starting version."))?;
            let from = r
                .versions
                .iter()
                .find(|v| v.revision == revision)
                .ok_or_else(|| Refusal::new("not_found", "No such version."))?;
            let to = r.versions.last().unwrap();
            Ok(json!({"key": k, "from_revision": revision, "to_revision": to.revision, "changes": differences(&from.data,&to.data,"")}))
        }
        Action::Read => {
            let k = key(a)?;
            let v = current(w, pid, k).ok_or_else(|| Refusal::new("not_found", "No such memory record."))?;
            let data = match &a.path {
                Some(path) => v.data.pointer(path).cloned(),
                None => Some(v.data.clone()),
            };
            Ok(
                json!({"key": k, "revision": v.revision, "tick": v.tick, "expires_tick": v.expires_tick, "archived": v.expires_tick.is_some_and(|t| t <= w.tick), "data": data}),
            )
        }
        Action::History => {
            let k = key(a)?;
            let r = p.memory.get(k).ok_or_else(|| Refusal::new("not_found", "No such memory record."))?;
            let offset = a.offset.unwrap_or(0).min(r.versions.len());
            let limit = a.limit.unwrap_or(10).clamp(1, 100);
            let end = offset.saturating_add(limit).min(r.versions.len());
            Ok(
                json!({"key": k, "versions": &r.versions[offset..end], "total": r.versions.len(), "continuation": {"has_more": end < r.versions.len(), "next_arguments": {"action": "history", "key": k, "offset": end, "limit": limit}}}),
            )
        }
        _ => Err(Refusal::new("invalid_args", "This memory action changes state.")),
    }
}
pub fn write(ctx: &mut Ctx, pid: PlayerId, a: &Args) -> Res<Outcome> {
    ctx.player(pid)?;
    if a.action == Action::Import {
        let bundle = a.data.as_ref().ok_or_else(|| Refusal::new("invalid_args", "data is required."))?;
        if bundle["format"] != "mcpwars-private-memory-1" {
            return Err(Refusal::new("invalid_args", "Unsupported memory export format."));
        }
        let notes: BTreeMap<String, String> =
            serde_json::from_value(bundle["notes"].clone()).map_err(|_| Refusal::new("invalid_args", "Invalid notes."))?;
        let records: BTreeMap<String, Record> =
            serde_json::from_value(bundle["records"].clone()).map_err(|_| Refusal::new("invalid_args", "Invalid records."))?;
        // Opt-in import never overwrites live work. Validate the entire bundle before mutation.
        if notes.len() + ctx.w.players[&pid].notes.len() > 40 || records.len() + ctx.w.players[&pid].memory.len() > 40 {
            return Err(Refusal::new("limit_reached", "Import exceeds 40 notes or records."));
        }
        for k in notes.keys() {
            if k.is_empty() || k.chars().count() > 40 || ctx.w.players[&pid].notes.contains_key(k) {
                return Err(Refusal::new("conflict", "Imported note keys must be valid and unused."));
            }
        }
        for (k, r) in &records {
            if k.is_empty()
                || k == "plan"
                || k.chars().count() > 40
                || ctx.w.players[&pid].memory.contains_key(k)
                || r.versions.is_empty()
                || r.versions.iter().enumerate().any(|(i, v)| v.revision != i as u64 + 1)
            {
                return Err(Refusal::new(
                    "conflict",
                    "Imported records must have valid unused keys and consecutive revisions.",
                ));
            }
        }
        let tick = ctx.w.tick;
        let p = ctx.player_mut(pid);
        if notes.contains_key("plan") {
            p.plan_tick = Some(tick);
        }
        p.notes.extend(notes);
        p.memory.extend(records);
        return Ok(ctx.finish("Imported private memory.", json!({"imported": true})));
    }
    if !matches!(a.action, Action::Save | Action::Patch | Action::Append) {
        return Err(Refusal::new("invalid_args", "Use a read action through the memory tool."));
    }
    let k = key(a)?.to_string();
    let old = current(ctx.w, pid, &k);
    let revision = old.map(|v| v.revision).unwrap_or(0);
    if matches!(a.action, Action::Patch | Action::Append) && (old.is_none() || a.expected_revision.is_none()) {
        return Err(Refusal::new(
            "invalid_args",
            "patch/append needs an existing record and expected_revision.",
        ));
    }
    if a.expected_revision.is_some_and(|r| r != revision) {
        return Err(Refusal::new(
            "conflict",
            format!("Current revision is {revision}; read it before retrying."),
        ));
    }
    if old.is_none() && ctx.w.players[&pid].memory.len() >= 40 {
        return Err(Refusal::new(
            "limit_reached",
            "You already have 40 structured records. Update an existing record.",
        ));
    }
    let mut data = a.data.clone().ok_or_else(|| Refusal::new("invalid_args", "data is required."))?;
    if a.action == Action::Patch {
        let mut next = old.unwrap().data.clone();
        merge(&mut next, &data);
        data = next;
    }
    if a.action == Action::Append {
        let mut next = old.unwrap().data.clone();
        let array = next
            .as_array_mut()
            .ok_or_else(|| Refusal::new("invalid_args", "append requires an array record."))?;
        array.push(data);
        data = next;
    }
    let expires_tick = a
        .ttl_ticks
        .map(|t| ctx.w.tick.saturating_add(t))
        .or_else(|| old.and_then(|v| v.expires_tick));
    let v = Version {
        revision: revision + 1,
        tick: ctx.w.tick,
        expires_tick,
        data,
    };
    ctx.player_mut(pid).memory.entry(k.clone()).or_default().versions.push(v);
    Ok(ctx.finish(
        format!("Saved private memory '{k}' revision {}.", revision + 1),
        json!({"key": k, "revision": revision + 1}),
    ))
}
