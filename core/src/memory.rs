//! Confidential working memory. Archived bodies remain in the durable store, outside the world.
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
    /// Archive for permanent operator history.
    Archive,
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
    /// Archive after this many ticks.
    #[serde(default)]
    pub ttl_ticks: Option<Tick>,
    /// JSON pointer for a nested read, e.g. /facts/0/sector.
    #[serde(default)]
    pub path: Option<String>,
    /// Ignored for captain reads.
    #[serde(default)]
    pub include_archived: bool,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}
impl Args {
    pub fn mutates(&self) -> bool {
        matches!(
            self.action,
            Action::Save | Action::Patch | Action::Append | Action::Import | Action::Archive
        )
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
pub fn current<'a>(w: &'a World, pid: PlayerId, k: &str) -> Option<&'a Version> {
    w.players
        .get(&pid)?
        .memory
        .get(k)?
        .versions
        .last()
        .filter(|v| v.expires_tick.is_none_or(|t| t > w.tick))
}
/// Forget expired working records; complete bodies already live in the permanent journal/archive.
pub fn expire(w: &mut World) {
    let tick = w.tick;
    for p in w.players.values_mut() {
        let expired: Vec<_> = p
            .memory
            .iter()
            .filter(|(_, r)| r.versions.last().is_some_and(|v| v.expires_tick.is_some_and(|t| t <= tick)))
            .map(|(k, _)| k.clone())
            .collect();
        for k in expired {
            retire(p, &k);
        }
    }
}
fn retire(p: &mut crate::model::Player, k: &str) {
    if let Some(r) = p.memory.remove(k) {
        let last = r.versions.last().map_or(0, |v| v.revision);
        let floor = p.memory_revisions.entry(k.to_string()).or_default();
        *floor = (*floor).max(last);
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
    read_inner(w, pid, a, false)
}
/// Trusted operator entry point. Captain-supplied include_archived never grants this access.
pub fn read_operator(w: &World, pid: PlayerId, a: &Args) -> Res<Value> {
    read_inner(w, pid, a, true)
}
fn read_inner(w: &World, pid: PlayerId, a: &Args, operator: bool) -> Res<Value> {
    let p = w
        .players
        .get(&pid)
        .ok_or_else(|| Refusal::new("unknown_player", "No such captain."))?;
    if let Some(k) = &a.key
        && !operator
        && matches!(a.action, Action::Read | Action::History | Action::Diff)
        && current(w, pid, k).is_none()
    {
        return Err(Refusal::new("not_found", "No active memory record."));
    }
    match a.action {
        Action::Export => Ok(
            json!({"format": "mcpwars-private-memory-1", "notes": p.notes, "records": p.memory.iter().filter(|(k,_)| operator || current(w,pid,k).is_some()).collect::<BTreeMap<_,_>>()}),
        ),
        Action::List => Ok(
            json!({"records": p.memory.iter().filter(|(_,r)| operator || r.versions.last().is_some_and(|v| v.expires_tick.is_none_or(|t| t > w.tick))).filter_map(|(k,r)| r.versions.last().map(|v| json!({"key": k, "revision": v.revision, "tick": v.tick, "expires_tick": v.expires_tick, "archived": v.expires_tick.is_some_and(|t| t <= w.tick)}))).collect::<Vec<_>>()}),
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
            let v = p
                .memory
                .get(k)
                .and_then(|r| r.versions.last())
                .ok_or_else(|| Refusal::new("not_found", "No such memory record."))?;
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
        let mut records: BTreeMap<String, Record> =
            serde_json::from_value(bundle["records"].clone()).map_err(|_| Refusal::new("invalid_args", "Invalid records."))?;
        // Opt-in import never overwrites live work. Validate the entire bundle before mutation.
        if notes.len() + ctx.w.players[&pid].notes.len() > 40
            || records.len()
                + ctx.w.players[&pid]
                    .memory
                    .keys()
                    .filter(|k| current(ctx.w, pid, k).is_some())
                    .count()
                > 40
        {
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
                || current(ctx.w, pid, k).is_some()
                || r.versions.is_empty()
                || r.versions[0].revision == 0
                || r.versions.windows(2).any(|v| v[0].revision.checked_add(1) != Some(v[1].revision))
                || r.versions.last().is_some_and(|v| v.expires_tick.is_some_and(|t| t <= ctx.w.tick))
            {
                return Err(Refusal::new(
                    "conflict",
                    "Imported records must have valid unused keys and consecutive revisions.",
                ));
            }
        }
        for (k, r) in &records {
            let floor = ctx.w.players[&pid].memory_revisions.get(k).copied().unwrap_or(0).max(
                ctx.w.players[&pid]
                    .memory
                    .get(k)
                    .and_then(|r| r.versions.last())
                    .map_or(0, |v| v.revision),
            );
            if floor == u64::MAX
                || (r.versions[0].revision <= floor
                    && r.versions
                        .last()
                        .unwrap()
                        .revision
                        .checked_add(floor.saturating_add(1).saturating_sub(r.versions[0].revision))
                        .is_none())
            {
                return Err(Refusal::new("limit_reached", "Revision limit reached."));
            }
        }
        let tick = ctx.w.tick;
        let p = ctx.player_mut(pid);
        if notes.contains_key("plan") {
            p.plan_tick = Some(tick);
        }
        p.notes.extend(notes);
        for (k, r) in &mut records {
            retire(p, k);
            let floor = p.memory_revisions.get(k).copied().unwrap_or(0);
            if r.versions[0].revision <= floor {
                let shift = floor.saturating_add(1).saturating_sub(r.versions[0].revision);
                for v in &mut r.versions {
                    v.revision += shift;
                }
            }
            p.memory_revisions.insert(k.clone(), r.versions.last().unwrap().revision);
        }
        p.memory.extend(records);
        return Ok(ctx.finish("Imported private memory.", json!({"imported": true})));
    }
    if a.action == Action::Archive {
        let k = key(a)?.to_string();
        let v = current(ctx.w, pid, &k).ok_or_else(|| Refusal::new("not_found", "No active memory record."))?;
        if a.expected_revision.is_some_and(|r| r != v.revision) {
            return Err(Refusal::new("conflict", "Revision changed; read before archiving."));
        }
        retire(ctx.player_mut(pid), &k);
        return Ok(ctx.finish(format!("Archived confidential memory '{k}'."), json!({"key":k,"archived":true})));
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
    if old.is_none()
        && ctx.w.players[&pid]
            .memory
            .keys()
            .filter(|k| current(ctx.w, pid, k).is_some())
            .count()
            >= 40
    {
        return Err(Refusal::new(
            "limit_reached",
            "You already have 40 active structured records. Archive a key or update an existing record.",
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
    let next_revision = ctx.w.players[&pid]
        .memory_revisions
        .get(&k)
        .copied()
        .unwrap_or(0)
        .max(revision)
        .max(
            ctx.w.players[&pid]
                .memory
                .get(&k)
                .and_then(|r| r.versions.last())
                .map_or(0, |v| v.revision),
        )
        .checked_add(1)
        .ok_or_else(|| Refusal::new("limit_reached", "Revision limit reached."))?;
    let v = Version {
        revision: next_revision,
        tick: ctx.w.tick,
        expires_tick,
        data,
    };
    let absent = old.is_none();
    let p = ctx.player_mut(pid);
    if absent {
        retire(p, &k);
    }
    p.memory_revisions.insert(k.clone(), next_revision);
    p.memory.entry(k.clone()).or_default().versions.push(v);
    Ok(ctx.finish(
        format!("Saved private memory '{k}' revision {}.", next_revision),
        json!({"key": k, "revision": next_revision}),
    ))
}
