//! Mentions (engine 1.58): `@Call Sign` and `@corp:Name` in what captains write.
//!
//! A mention is stored on the item it was written in (as ids, always shown under current names)
//! and tells the captains it names who can read the item, within a per-sender limit. One write
//! sends at most one notice per captain: a mention rides free on a notice the captain gets anyway
//! (an author, citation or electorate notice). Captains may mute a sender or a corporation; the
//! sender is not told. `World.mentions` keeps the mutes, the limit's window, the operator's
//! mention graph (one row per season and pair, bounded per season), the newest edges and the
//! public "Mentioned in" index. Nothing clears it.
//!
//! Resolution is pure and runs before any mutation: a write resolves, plans (who is told, what
//! rides along, the limit, mutes, excerpts), stores its item, sends, then commits the record.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::{Ctx, NoticeExtra, Outcome, Refusal, Res};
use crate::model::{CorpId, Fold, Player, PlayerId, PlayerKind, SYSOP, Tick, World};

/// Distinct targets one item may store.
pub const TARGETS_MAX: usize = 20;
/// Characters of an excerpt carried on a notice.
pub const EXCERPT_CHARS: usize = 200;
/// Characters kept before the `@` in an excerpt.
pub const EXCERPT_LEAD: usize = 60;
/// Recent edges kept for the operator.
pub const EDGES_MAX: usize = 1_000;
/// Graph rows one season may hold; later new pairs count in that season's overflow.
pub const GRAPH_ROWS_PER_SEASON: usize = 5_000;
/// Public places kept per target.
pub const INDEX_KEEP: usize = 60;
/// Places shown per public profile.
pub const MENTIONED_IN_MAX: usize = 30;
/// Mutes per captain, captains and corporations together.
pub const MUTES_MAX: usize = 200;
/// Characters of one structured entry.
pub const ENTRY_MAX: usize = 64;
/// The number of `Surface`s.
pub const SURFACES: usize = 6;
/// The longest name a mention can match (call signs and corporation names are at most 24).
const WINDOW_CHARS: usize = 24;
/// What moderation leaves in place of a copy.
pub const STRUCK: &str = crate::chronicle::STRUCK;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum Target {
    Captain(PlayerId),
    Corp(CorpId),
}

/// A corporation as mentioned: its id and its name when written (a fresh start clears
/// corporations, so the name is kept).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpMention {
    pub id: CorpId,
    pub name: String,
}

/// Stored on an item: first-appearance order, deduplicated, at most `TARGETS_MAX`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mentions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captains: Vec<PlayerId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corps: Vec<CorpMention>,
}

impl Mentions {
    pub fn is_empty(&self) -> bool {
        self.captains.is_empty() && self.corps.is_empty()
    }

    /// Captains, then corporations, in stored order.
    pub fn targets(&self) -> Vec<Target> {
        self.captains
            .iter()
            .map(|c| Target::Captain(*c))
            .chain(self.corps.iter().map(|c| Target::Corp(c.id)))
            .collect()
    }

    pub fn hits(&self, t: Target) -> bool {
        match t {
            Target::Captain(c) => self.captains.contains(&c),
            Target::Corp(k) => self.corps.iter().any(|c| c.id == k),
        }
    }

    /// Current names: a removed captain reads `#id`; a corporation `corp:Name` under its current
    /// name, else the name it had when written.
    pub fn names(&self, w: &World) -> Vec<String> {
        self.captains
            .iter()
            .map(|c| captain_name(w, *c))
            .chain(self.corps.iter().map(|c| format!("corp:{}", corp_name(w, c.id, Some(&c.name)))))
            .collect()
    }

    /// Names for public views: removed captains and NPCs are left out.
    pub fn public_names(&self, w: &World) -> Vec<String> {
        self.captains
            .iter()
            .filter_map(|c| w.players.get(c).filter(|p| is_captain(p)).map(|p| p.name.clone()))
            .chain(self.corps.iter().map(|c| format!("corp:{}", corp_name(w, c.id, Some(&c.name)))))
            .collect()
    }
}

/// Where a mention was written. Indexes `GraphRow::by_surface`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    PublicRadio,
    CorpRadio,
    Page,
    Talk,
    Log,
    Proposal,
}

impl Surface {
    pub const ALL: [Surface; SURFACES] = [
        Surface::PublicRadio,
        Surface::CorpRadio,
        Surface::Page,
        Surface::Talk,
        Surface::Log,
        Surface::Proposal,
    ];

    pub fn index(self) -> usize {
        match self {
            Surface::PublicRadio => 0,
            Surface::CorpRadio => 1,
            Surface::Page => 2,
            Surface::Talk => 3,
            Surface::Log => 4,
            Surface::Proposal => 5,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Surface::PublicRadio => "public_radio",
            Surface::CorpRadio => "corp_radio",
            Surface::Page => "page",
            Surface::Talk => "talk",
            Surface::Log => "log",
            Surface::Proposal => "proposal",
        }
    }
}

/// Where something was written. Also the locator on notices (`Notice.about`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "surface")]
pub enum Place {
    /// `corp`: set on corporation radio.
    Radio {
        id: u64,
        corp: Option<CorpId>,
    },
    Page {
        page: u64,
        revision: u32,
    },
    Talk {
        page: u64,
        comment: u64,
    },
    Log {
        owner: PlayerId,
        n: u32,
    },
    Proposal {
        id: u64,
    },
}

/// The item a place (or a moderation action) is about. A page and its talk are one item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Item {
    Radio(u64),
    Page(u64),
    Proposal(u64),
    Article(u64),
    Log(PlayerId, u32),
}

impl Place {
    pub fn surface(&self) -> Surface {
        match self {
            Place::Radio { corp: None, .. } => Surface::PublicRadio,
            Place::Radio { corp: Some(_), .. } => Surface::CorpRadio,
            Place::Page { .. } => Surface::Page,
            Place::Talk { .. } => Surface::Talk,
            Place::Log { .. } => Surface::Log,
            Place::Proposal { .. } => Surface::Proposal,
        }
    }

    /// A short description, for example "public radio (message 812)".
    pub fn label(&self) -> String {
        match self {
            Place::Radio { id, corp: None } => format!("public radio (message {id})"),
            Place::Radio { id, corp: Some(_) } => format!("corporation radio (message {id})"),
            Place::Page { page, revision } => format!("Chronicle page {page}, revision {revision}"),
            Place::Talk { page, comment } => format!("the talk of Chronicle page {page} (comment {comment})"),
            Place::Log { n, .. } => format!("a public log line (entry {n})"),
            Place::Proposal { id } => format!("constitution proposal #{id}"),
        }
    }

    pub fn item(&self) -> Item {
        match self {
            Place::Radio { id, .. } => Item::Radio(*id),
            Place::Page { page, .. } | Place::Talk { page, .. } => Item::Page(*page),
            Place::Log { owner, n } => Item::Log(*owner, *n),
            Place::Proposal { id } => Item::Proposal(*id),
        }
    }
}

/// On a notice: the captain (or its corporation) was mentioned. The place is `Notice.about`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionRef {
    pub from: PlayerId,
    pub from_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via_corp: Option<CorpMention>,
}

/// What became of one target of one write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fate {
    /// Told with a new notice (one unit of the sender's limit).
    Notified,
    /// Told only on notices its recipients got anyway (free).
    RodeAlong,
    /// The target cannot read the item.
    CannotRead,
    /// The sender's limit was used up.
    OverCap,
    /// Already mentioned by the item's previous revision.
    Repeat,
}

impl Fate {
    pub fn name(self) -> &'static str {
        match self {
            Fate::Notified => "notified",
            Fate::RodeAlong => "rode_along",
            Fate::CannotRead => "cannot_read",
            Fate::OverCap => "over_cap",
            Fate::Repeat => "already_mentioned",
        }
    }
}

/// One mention, as the operator's recent list keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub tick: Tick,
    pub at_ms: u64,
    pub season: u32,
    pub epoch: u32,
    pub from: PlayerId,
    pub to: Target,
    pub at: Place,
    pub fate: Fate,
    /// Recipients told.
    #[serde(default)]
    pub delivered: u32,
    /// Recipients who muted the sender.
    #[serde(default)]
    pub muted: u32,
}

/// One cell of the permanent mention graph: one row per (epoch, season, from, to).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphRow {
    pub epoch: u32,
    pub season: u32,
    pub from: PlayerId,
    pub to: Target,
    /// Indexed by `Surface::index`.
    pub by_surface: [u64; SURFACES],
    pub count: u64,
    pub notified: u64,
    pub muted: u64,
    pub first_tick: Tick,
    pub last_tick: Tick,
}

/// Mentions that found no graph row because the season's rows were full.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Overflow {
    pub epoch: u32,
    pub season: u32,
    pub count: u64,
}

/// A captain's mutes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mutes {
    #[serde(default)]
    pub captains: BTreeSet<PlayerId>,
    #[serde(default)]
    pub corps: BTreeSet<CorpId>,
}

impl Mutes {
    pub fn len(&self) -> usize {
        self.captains.len() + self.corps.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// `World.mentions` (engine 1.58). Never cleared: not by a Big Bang, a fresh start or a Chronicle clear.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub mutes: BTreeMap<PlayerId, Mutes>,
    /// Per sender: one tick per limit unit spent, pruned to `mention_window_ticks` on each commit.
    #[serde(default)]
    pub recent: BTreeMap<PlayerId, Vec<Tick>>,
    /// Sorted by (epoch, season, from, to); upserted by binary search.
    #[serde(default)]
    pub graph: Vec<GraphRow>,
    #[serde(default)]
    pub overflow: Vec<Overflow>,
    /// The newest `EDGES_MAX` edges.
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// Public places (readers = everyone) naming each target, newest last, `INDEX_KEEP` each.
    #[serde(default)]
    pub by_captain: BTreeMap<PlayerId, Vec<Place>>,
    #[serde(default)]
    pub by_corp: BTreeMap<CorpId, Vec<Place>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    List,
    Mute,
    Unmute,
}

/// The `mentions` captain tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Args {
    pub action: Action,
    /// mute, unmute: "Call Sign" or "corp:Name".
    #[serde(default)]
    pub target: Option<String>,
}

impl Args {
    pub fn mutates(&self) -> bool {
        !matches!(self.action, Action::List)
    }
}

// ---------------------------------------------------------------------------------------------
// Names

/// A captain that can be mentioned: not an NPC and not the operator.
fn is_captain(p: &Player) -> bool {
    p.npc.is_none() && p.kind != PlayerKind::Npc && p.id != SYSOP
}

/// A captain's current call sign, or `#id` once removed.
pub fn captain_name(w: &World, id: PlayerId) -> String {
    w.players.get(&id).map(|p| p.name.clone()).unwrap_or_else(|| format!("#{id}"))
}

/// A corporation's current name, else the stored name, else `#id`.
pub fn corp_name(w: &World, id: CorpId, stored: Option<&str>) -> String {
    w.corps
        .get(&id)
        .map(|c| c.name.clone())
        .or_else(|| stored.map(str::to_string))
        .unwrap_or_else(|| format!("#{id}"))
}

/// A target's display name: a captain's call sign, or `corp:Name`.
pub fn target_name(w: &World, t: Target) -> String {
    match t {
        Target::Captain(c) => captain_name(w, c),
        Target::Corp(k) => format!("corp:{}", corp_name(w, k, None)),
    }
}

/// Lowercase ASCII alphanumeric runs joined by one space: "Quiet-Comet", "quiet comet" and
/// "Quiet_Comet" are all `quiet comet`, while "Vexate" and "Vex ate" differ.
pub fn word_key(s: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    out
}

/// The names mentions resolve against, built once per write or read.
#[derive(Clone, Debug, Default)]
pub struct Index {
    /// `word_key` -> (rank, target): rank 0 current call signs, 1 former ones, 2 corporations.
    pub inline: BTreeMap<String, (u8, Target)>,
    /// `player::name_key` -> (rank, target).
    pub exact: BTreeMap<String, (u8, Target)>,
    pub corps_inline: BTreeMap<String, CorpId>,
    pub corps_exact: BTreeMap<String, CorpId>,
}

fn put<K: Ord>(m: &mut BTreeMap<K, (u8, Target)>, k: K, rank: u8, t: Target) {
    match m.get(&k) {
        Some((r, _)) if *r <= rank => {}
        _ => {
            m.insert(k, (rank, t));
        }
    }
}

/// Build the name index: current call signs beat former ones, which beat corporation names;
/// within a rank the lowest id wins. Banned captains are included; NPCs and the operator are not.
pub fn index(w: &World) -> Index {
    let mut ix = Index::default();
    let caps: Vec<&Player> = w.players.values().filter(|p| is_captain(p)).collect();
    for p in &caps {
        let t = Target::Captain(p.id);
        for (k, e) in [(word_key(&p.name), false), (crate::player::name_key(&p.name), true)] {
            if k.is_empty() {
                continue;
            }
            if e {
                put(&mut ix.exact, k, 0, t);
            } else {
                put(&mut ix.inline, k, 0, t);
            }
        }
    }
    for p in &caps {
        let t = Target::Captain(p.id);
        for f in w.identities.get(&p.id).map(|i| i.former_names.as_slice()).unwrap_or(&[]) {
            let (wk, nk) = (word_key(f), crate::player::name_key(f));
            if !wk.is_empty() {
                put(&mut ix.inline, wk, 1, t);
            }
            if !nk.is_empty() {
                put(&mut ix.exact, nk, 1, t);
            }
        }
    }
    for c in w.corps.values() {
        let t = Target::Corp(c.id);
        let (wk, nk) = (word_key(&c.name), crate::player::name_key(&c.name));
        if !wk.is_empty() {
            put(&mut ix.inline, wk.clone(), 2, t);
            ix.corps_inline.entry(wk).or_insert(c.id);
        }
        if !nk.is_empty() {
            put(&mut ix.exact, nk.clone(), 2, t);
            ix.corps_exact.entry(nk).or_insert(c.id);
        }
    }
    ix
}

fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '\'' | '.')
}

/// The longest name starting at `start` (a char offset): returns the target and the offset just
/// past the match. A candidate must end on an ASCII letter or digit not followed by another
/// alphanumeric character, so a match never ends inside a word.
fn longest(chars: &[char], start: usize, map: impl Fn(&str) -> Option<Target>) -> Option<(Target, usize)> {
    let len = chars[start..].iter().take(WINDOW_CHARS).take_while(|c| name_char(**c)).count();
    for e in (1..=len).rev() {
        let end = start + e;
        if !chars[end - 1].is_ascii_alphanumeric() {
            continue;
        }
        if chars.get(end).is_some_and(|c| c.is_alphanumeric()) {
            continue;
        }
        let cand: String = chars[start..end].iter().collect();
        if let Some(t) = map(&word_key(&cand)) {
            return Some((t, end));
        }
    }
    None
}

/// Inline mentions: `@Name` or `@corp:Name`, with the char offset of each `@`. An `@` counts only
/// at the start or after a character that is not alphanumeric (`a@b.c` is no mention). A name
/// that matches nothing stays plain text.
pub fn inline(text: &str, ix: &Index) -> Vec<(Target, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '@' || (i > 0 && chars[i - 1].is_alphanumeric()) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let corp_only = chars.len() >= j + 5 && chars[j..j + 5].iter().collect::<String>().eq_ignore_ascii_case("corp:");
        if corp_only {
            j += 5;
        }
        // The name starts right after the `@` (or `corp:`) with a letter or digit: "meet @ Vex"
        // and "@-Vex" are plain text, never a mention that spends the sender's limit.
        let hit = if chars.get(j).is_some_and(|c| c.is_ascii_alphanumeric()) {
            if corp_only {
                longest(&chars, j, |k| ix.corps_inline.get(k).map(|c| Target::Corp(*c)))
            } else {
                longest(&chars, j, |k| ix.inline.get(k).map(|(_, t)| *t))
            }
        } else {
            None
        };
        match hit {
            Some((t, end)) => {
                out.push((t, i));
                i = end;
            }
            None => i += 1,
        }
    }
    out
}

/// Bare names in a text (news headlines): every word start, longest match, no `@` needed.
/// With captains "Vex" and "Vex Arden", "Vex Arden won the race" names Vex Arden only.
pub fn names_in(text: &str, ix: &Index) -> BTreeSet<Target> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < chars.len() {
        let start = chars[i].is_ascii_alphanumeric() && (i == 0 || !chars[i - 1].is_alphanumeric());
        if start && let Some((t, end)) = longest(&chars, i, |k| ix.inline.get(k).map(|(_, t)| *t)) {
            out.insert(t);
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let sub = prev[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Up to three current names close to an unknown entry: captains, then `corp:` names.
pub fn closest(w: &World, entry: &str) -> Vec<String> {
    let key = crate::player::name_key(entry_name(entry).1);
    if key.is_empty() {
        return vec![];
    }
    let n = key.chars().count();
    let mut cands: Vec<(usize, u8, String)> = vec![];
    let mut consider = |name: String, k: String, kind: u8| {
        if k.is_empty() {
            return;
        }
        let d = levenshtein(&key, &k);
        if d <= (n / 3).max(2) || k.starts_with(&key) || key.starts_with(&k) {
            cands.push((d, kind, name));
        }
    };
    for p in w.players.values().filter(|p| is_captain(p)) {
        consider(p.name.clone(), crate::player::name_key(&p.name), 0);
    }
    for c in w.corps.values() {
        consider(format!("corp:{}", c.name), crate::player::name_key(&c.name), 1);
    }
    cands.sort();
    cands.dedup_by(|a, b| a.2 == b.2);
    cands.into_iter().take(3).map(|c| c.2).collect()
}

/// An entry without one leading `@`, and whether it names a corporation (`corp:`).
fn entry_name(entry: &str) -> (bool, &str) {
    let e = entry.trim();
    let e = e.strip_prefix('@').unwrap_or(e).trim_start();
    if e.len() >= 5 && e.is_char_boundary(5) && e[..5].eq_ignore_ascii_case("corp:") {
        (true, e[5..].trim())
    } else {
        (false, e)
    }
}

fn unknown(w: &World, entry: &str) -> Refusal {
    let near = closest(w, entry);
    let hint = if near.is_empty() {
        "get_briefing comms.captains lists captains.".to_string()
    } else {
        format!("Closest: {}.", near.join(", "))
    };
    Refusal::new(
        "unknown_mention",
        format!("No captain or corporation is named '{entry}'. Nothing was sent or changed."),
    )
    .hint(hint)
}

/// One structured entry ("Call Sign", "@Call Sign" or "corp:Name"), matched exactly by
/// `player::name_key` (case and punctuation ignored, no prefixes).
pub fn entry(w: &World, ix: &Index, entry: &str) -> Res<Target> {
    if entry.chars().count() > ENTRY_MAX {
        return Err(unknown(w, &entry.chars().take(ENTRY_MAX).collect::<String>()));
    }
    let (corp, name) = entry_name(entry);
    let key = crate::player::name_key(name);
    let hit = if key.is_empty() {
        None
    } else if corp {
        ix.corps_exact.get(&key).map(|c| Target::Corp(*c))
    } else {
        ix.exact.get(&key).map(|(_, t)| *t)
    };
    hit.ok_or_else(|| unknown(w, entry.trim()))
}

/// Structured mentions, deduplicated in order. More than `TARGETS_MAX` entries, or one that names
/// no captain or corporation, refuses the whole write.
pub fn structured(w: &World, ix: &Index, entries: &[String]) -> Res<Vec<Target>> {
    if entries.len() > TARGETS_MAX {
        return Err(Refusal::new(
            "too_many_mentions",
            format!("{} mentions given; nothing was sent or changed.", entries.len()),
        )
        .hint(format!("At most {TARGETS_MAX} per item.")));
    }
    let mut out: Vec<Target> = vec![];
    for e in entries {
        let t = entry(w, ix, e)?;
        if !out.contains(&t) {
            out.push(t);
        }
    }
    Ok(out)
}

/// What a write mentions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resolution {
    pub mentions: Mentions,
    /// The first inline mention of each target: (index into the texts, char offset of the `@`).
    pub offsets: BTreeMap<Target, (usize, usize)>,
    /// Targets given in the structured list.
    pub structured: BTreeSet<Target>,
    /// Inline targets beyond `TARGETS_MAX`, dropped (never refused).
    pub ignored: usize,
}

impl Resolution {
    /// Whether the reply should carry a `mentions` block.
    pub fn any(&self) -> bool {
        !self.mentions.is_empty() || self.ignored > 0
    }
}

/// Resolve a write's mentions: the structured list first, then inline mentions across `texts`
/// in order. The sender's own id is stored but never notified or recorded.
pub fn resolve(w: &World, structured_entries: Option<&[String]>, texts: &[&str]) -> Res<Resolution> {
    let ix = index(w);
    let mut res = Resolution::default();
    let mut order: Vec<Target> = vec![];
    if let Some(es) = structured_entries {
        for t in structured(w, &ix, es)? {
            res.structured.insert(t);
            order.push(t);
        }
    }
    let mut dropped: BTreeSet<Target> = BTreeSet::new();
    for (ti, text) in texts.iter().enumerate() {
        for (t, off) in inline(text, &ix) {
            if !order.contains(&t) {
                if order.len() >= TARGETS_MAX {
                    dropped.insert(t);
                    continue;
                }
                order.push(t);
            }
            res.offsets.entry(t).or_insert((ti, off));
        }
    }
    res.ignored = dropped.len();
    for t in order {
        match t {
            Target::Captain(c) => res.mentions.captains.push(c),
            Target::Corp(k) => res.mentions.corps.push(CorpMention {
                id: k,
                name: corp_name(w, k, None),
            }),
        }
    }
    Ok(res)
}

// ---------------------------------------------------------------------------------------------
// Planning, sending and recording

/// Who can read an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readers {
    Everyone,
    /// The corporation's members at the time of writing.
    CorpMembers(CorpId),
    Nobody,
}

/// A notice a write sends anyway (an author, citation or electorate notice): a mention rides on it.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub kind: &'static str,
    /// Server-written text only.
    pub text: String,
    /// Player words, shown as untrusted player text.
    pub quote: Option<String>,
    pub about: Option<Place>,
    pub fold: Option<Fold>,
}

/// Who a write tells, and how.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub from: PlayerId,
    /// Each target (self excluded) with its fate and the recipients it reached.
    pub per_target: Vec<(Target, Fate, u32)>,
    /// Recipient -> (the target that reached it, delivered?); delivered = false means muted.
    pub recipients: BTreeMap<PlayerId, (Target, bool)>,
    /// Excerpts per target, cut from the (already redacted) texts.
    pub excerpts: BTreeMap<Target, String>,
    /// Limit units this write spends.
    pub units: u32,
    pub over_cap: bool,
    /// Units left after this write.
    pub left_after: u32,
    /// Everyone can read the item (it enters the public index).
    pub public: bool,
}

fn in_window(w: &World, t: Tick) -> bool {
    w.tick.saturating_sub(t) < w.rules.mention_window_ticks
}

/// Limit units the sender has left in the current window.
pub fn left(w: &World, from: PlayerId) -> u32 {
    let used = w
        .mentions
        .recent
        .get(&from)
        .map(|v| v.iter().filter(|t| in_window(w, **t)).count())
        .unwrap_or(0);
    w.rules.mention_notify_max.saturating_sub(used as u32)
}

fn muted_by(w: &World, recipient: PlayerId, from: PlayerId) -> bool {
    let Some(m) = w.mentions.mutes.get(&recipient) else {
        return false;
    };
    m.captains.contains(&from) || w.players.get(&from).and_then(|p| p.corp).is_some_and(|c| m.corps.contains(&c))
}

/// An excerpt of at most `EXCERPT_CHARS` characters around a char offset, on one line, marked
/// with three ASCII dots where it was cut.
pub fn excerpt(text: &str, off: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = off.saturating_sub(EXCERPT_LEAD).min(chars.len());
    let end = (start + EXCERPT_CHARS).min(chars.len());
    let mut s: String = chars[start..end]
        .iter()
        .map(|c| if matches!(c, '\n' | '\r') { ' ' } else { *c })
        .collect();
    if start > 0 {
        s.insert_str(0, "...");
    }
    if end < chars.len() {
        s.push_str("...");
    }
    s
}

/// Plan a write: who is told, what rides along, the limit, mutes and excerpts. Pure. `others`
/// holds the notices the write sends anyway; `texts` the texts resolved, the last one being the
/// main text (structured-only targets are excerpted from its start); `prior` the mentions of the
/// previous revision, which are not told again.
pub fn plan(
    w: &World,
    from: PlayerId,
    res: &Resolution,
    readers: Readers,
    prior: &Mentions,
    others: &BTreeMap<PlayerId, Pending>,
    texts: &[&str],
) -> Plan {
    let mut left = left(w, from);
    let mut plan = Plan {
        from,
        public: matches!(readers, Readers::Everyone),
        ..Plan::default()
    };
    for t in res.mentions.targets() {
        if t == Target::Captain(from) {
            continue;
        }
        if prior.hits(t) {
            plan.per_target.push((t, Fate::Repeat, 0));
            continue;
        }
        let readable: Option<Vec<PlayerId>> = match t {
            Target::Captain(c) => {
                let ok = match readers {
                    Readers::Everyone => true,
                    Readers::CorpMembers(k) => w.players.get(&c).is_some_and(|p| p.corp == Some(k)),
                    Readers::Nobody => false,
                };
                ok.then(|| vec![c])
            }
            Target::Corp(k) => {
                let ok = match readers {
                    Readers::Everyone => true,
                    Readers::CorpMembers(r) => r == k,
                    Readers::Nobody => false,
                };
                ok.then(|| {
                    let members: BTreeSet<PlayerId> = w.corps.get(&k).map(|c| c.members.iter().copied().collect()).unwrap_or_default();
                    members
                        .into_iter()
                        .filter(|m| *m != from && w.players.get(m).is_some_and(is_captain))
                        .collect()
                })
            }
        };
        let Some(rs) = readable else {
            plan.per_target.push((t, Fate::CannotRead, 0));
            continue;
        };
        let rs: Vec<PlayerId> = rs.into_iter().filter(|r| !plan.recipients.contains_key(r)).collect();
        let (riders, new): (Vec<PlayerId>, Vec<PlayerId>) = rs.into_iter().partition(|r| others.contains_key(r));
        let (fate, reached) = if !new.is_empty() && left == 0 {
            plan.over_cap = true;
            (Fate::OverCap, riders)
        } else if !new.is_empty() {
            left -= 1;
            plan.units += 1;
            (Fate::Notified, riders.into_iter().chain(new).collect())
        } else if !riders.is_empty() {
            (Fate::RodeAlong, riders)
        } else {
            // A corporation with no one else to tell.
            (Fate::Notified, vec![])
        };
        let mut delivered = false;
        for r in &reached {
            let ok = !muted_by(w, *r, from);
            delivered |= ok;
            plan.recipients.insert(*r, (t, ok));
        }
        plan.per_target.push((t, fate, reached.len() as u32));
        if delivered {
            let (ti, off) = res.offsets.get(&t).copied().unwrap_or((texts.len().saturating_sub(1), 0));
            plan.excerpts.insert(t, excerpt(texts.get(ti).copied().unwrap_or(""), off));
        }
    }
    plan.left_after = left;
    plan
}

/// "you" or "your corporation Name".
fn you(w: &World, t: Target) -> String {
    match t {
        Target::Captain(_) => "you".into(),
        Target::Corp(k) => format!("your corporation {}", corp_name(w, k, None)),
    }
}

/// The server text of a mention notice (kind "mention"). `from` is the sender's current call
/// sign and `you` is "you" or "your corporation Name": both server-validated.
pub fn notice_text(from: &str, you: &str, place: &Place) -> String {
    match place {
        Place::Radio { id, corp: None } => {
            format!("{from} mentioned {you} on public radio (message {id}). read_inbox kind=mention shows the excerpt.")
        }
        Place::Radio { id, corp: Some(_) } => {
            format!("{from} mentioned {you} on corporation radio (message {id}). read_inbox kind=mention shows the excerpt.")
        }
        Place::Page { page, revision } => {
            format!("{from} mentioned {you} on Chronicle page {page}, revision {revision}. chronicle action=read page={page}.")
        }
        Place::Talk { page, comment } => {
            format!("{from} mentioned {you} in the talk of Chronicle page {page} (comment {comment}). chronicle action=read page={page}.")
        }
        Place::Log { n, .. } => {
            format!("{from} mentioned {you} in a public log line (entry {n}). identity action=story name={from}.")
        }
        Place::Proposal { id } => format!("{from} mentioned {you} in constitution proposal #{id}. constitution action=view proposal={id}."),
    }
}

fn mention_ref(w: &World, from: PlayerId, from_name: &str, t: Target) -> MentionRef {
    MentionRef {
        from,
        from_name: from_name.to_string(),
        via_corp: match t {
            Target::Corp(k) => Some(CorpMention {
                id: k,
                name: corp_name(w, k, None),
            }),
            Target::Captain(_) => None,
        },
    }
}

/// Send a write's notices in recipient-id order: one per captain. A delivered rider gets its
/// pending notice with the mention mark (never folded, so a mention is never folded away); a
/// delivered new recipient gets a "mention" notice; every other pending notice goes as it is.
pub fn send(ctx: &mut Ctx, from: PlayerId, from_name: &str, plan: &Plan, place: Place, mut others: BTreeMap<PlayerId, Pending>) {
    let ids: BTreeSet<PlayerId> = plan.recipients.keys().chain(others.keys()).copied().collect();
    for pid in ids {
        let pending = others.remove(&pid);
        let mark = plan.recipients.get(&pid).filter(|(_, delivered)| *delivered).map(|(t, _)| *t);
        match (mark, pending) {
            (Some(t), Some(p)) => {
                let tail = match t {
                    Target::Captain(_) => " It mentions you.".to_string(),
                    Target::Corp(k) => format!(" It mentions your corporation {}.", corp_name(ctx.w, k, None)),
                };
                let extra = NoticeExtra {
                    quote: plan.excerpts.get(&t).cloned(),
                    about: p.about.or_else(|| Some(place.clone())),
                    mention: Some(mention_ref(ctx.w, from, from_name, t)),
                    fold: None,
                };
                ctx.notify_with(pid, p.kind, format!("{}{tail}", p.text), extra);
            }
            (Some(t), None) => {
                let text = notice_text(from_name, &you(ctx.w, t), &place);
                let extra = NoticeExtra {
                    quote: plan.excerpts.get(&t).cloned(),
                    about: Some(place.clone()),
                    mention: Some(mention_ref(ctx.w, from, from_name, t)),
                    fold: None,
                };
                ctx.notify_with(pid, "mention", text, extra);
            }
            (None, Some(p)) => send_pending(ctx, pid, p),
            (None, None) => {}
        }
    }
}

/// Send a pending notice as it is: folding when it carries a fold. A fold's text is rebuilt
/// from the merged range by `fold_text`.
pub fn send_pending(ctx: &mut Ctx, pid: PlayerId, p: Pending) {
    match p.fold {
        Some(f) => {
            ctx.notify_fold(pid, p.kind, f, fold_text, p.quote, p.about);
        }
        None => {
            ctx.notify_with(
                pid,
                p.kind,
                p.text,
                NoticeExtra {
                    quote: p.quote,
                    about: p.about,
                    mention: None,
                    fold: None,
                },
            );
        }
    }
}

fn names_list(w: &World, by: &[PlayerId]) -> String {
    let names: Vec<String> = by.iter().map(|p| captain_name(w, *p)).collect();
    match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        n => format!("{} and {}", names[..n - 1].join(", "), names[n - 1]),
    }
}

/// The server text of a folded notice, from its merged range.
pub fn fold_text(w: &World, f: &Fold) -> String {
    match f {
        Fold::Language {
            language,
            from_version,
            to_version,
            by,
        } => {
            let range = if from_version >= to_version {
                format!("a new version {to_version}")
            } else {
                format!("new versions {from_version} to {to_version}")
            };
            format!(
                "Language #{language} has {range} by {}. language action=read id={language} version={to_version}; language action=adopt id={language} marks it known.",
                names_list(w, by)
            )
        }
        Fold::PageRevised {
            page,
            from_revision,
            to_revision,
            by,
        } => {
            let range = if from_revision >= to_revision {
                format!("a new revision {to_revision}")
            } else {
                format!("new revisions {from_revision} to {to_revision}")
            };
            format!(
                "Chronicle page {page}, which you wrote or endorsed, has {range} by {}. chronicle action=read page={page}.",
                names_list(w, by)
            )
        }
        Fold::Talk { page, comments, by, .. } => {
            let n = if *comments == 1 {
                "1 new comment".to_string()
            } else {
                format!("{comments} new comments")
            };
            format!(
                "Chronicle page {page}, which you wrote, has {n} by {}. chronicle action=read page={page}.",
                names_list(w, by)
            )
        }
    }
}

fn row_key(r: &GraphRow) -> (u32, u32, PlayerId, Target) {
    (r.epoch, r.season, r.from, r.to)
}

/// Count one mention in the graph: upsert its (epoch, season, from, to) row, or count it in the
/// season's overflow when the season's rows are full.
pub fn record(st: &mut State, epoch: u32, season: u32, from: PlayerId, to: Target, add: &GraphRow) {
    let key = (epoch, season, from, to);
    match st.graph.binary_search_by(|r| row_key(r).cmp(&key)) {
        Ok(i) => {
            let r = &mut st.graph[i];
            for (a, b) in r.by_surface.iter_mut().zip(add.by_surface.iter()) {
                *a += b;
            }
            r.count += add.count;
            r.notified += add.notified;
            r.muted += add.muted;
            r.first_tick = r.first_tick.min(add.first_tick);
            r.last_tick = r.last_tick.max(add.last_tick);
        }
        Err(i) => {
            let lo = st.graph.partition_point(|r| (r.epoch, r.season) < (epoch, season));
            let hi = st.graph.partition_point(|r| (r.epoch, r.season) <= (epoch, season));
            if hi - lo >= GRAPH_ROWS_PER_SEASON {
                match st.overflow.iter_mut().find(|o| o.epoch == epoch && o.season == season) {
                    Some(o) => o.count += add.count,
                    None => st.overflow.push(Overflow {
                        epoch,
                        season,
                        count: add.count,
                    }),
                }
                return;
            }
            let mut row = add.clone();
            row.epoch = epoch;
            row.season = season;
            row.from = from;
            row.to = to;
            st.graph.insert(i, row);
        }
    }
}

/// Add a public place to a target's index, newest last, `INDEX_KEEP` kept.
pub fn index_place(st: &mut State, t: Target, place: &Place) {
    let v = match t {
        Target::Captain(c) => st.by_captain.entry(c).or_default(),
        Target::Corp(k) => st.by_corp.entry(k).or_default(),
    };
    v.push(place.clone());
    let excess = v.len().saturating_sub(INDEX_KEEP);
    if excess > 0 {
        v.drain(0..excess);
    }
}

/// Record a write that succeeded: the limit units, the graph, the recent edges and (for public
/// items) the public index. Call after `send`.
pub fn commit(ctx: &mut Ctx, from: PlayerId, plan: &Plan, place: &Place) {
    let (now, at_ms, season, epoch) = (ctx.w.tick, ctx.w.now_ms, ctx.w.season.number, ctx.w.archive.epoch);
    let window = ctx.w.rules.mention_window_ticks;
    let st = &mut ctx.w.mentions;
    {
        let recent = st.recent.entry(from).or_default();
        recent.retain(|t| now.saturating_sub(*t) < window);
        for _ in 0..plan.units {
            recent.push(now);
        }
        if recent.is_empty() {
            st.recent.remove(&from);
        }
    }
    let surface = place.surface().index();
    for (t, fate, _) in &plan.per_target {
        if *fate == Fate::Repeat || *t == Target::Captain(from) {
            continue;
        }
        let through = || plan.recipients.values().filter(move |(x, _)| x == t);
        let delivered = through().filter(|(_, d)| *d).count() as u32;
        let muted = through().filter(|(_, d)| !*d).count() as u32;
        let mut by_surface = [0; SURFACES];
        by_surface[surface] = 1;
        let add = GraphRow {
            epoch,
            season,
            from,
            to: *t,
            by_surface,
            count: 1,
            notified: u64::from(delivered > 0),
            muted: u64::from(muted),
            first_tick: now,
            last_tick: now,
        };
        record(st, epoch, season, from, *t, &add);
        st.edges.push(Edge {
            tick: now,
            at_ms,
            season,
            epoch,
            from,
            to: *t,
            at: place.clone(),
            fate: *fate,
            delivered,
            muted,
        });
        if plan.public {
            index_place(st, *t, place);
        }
    }
    let excess = st.edges.len().saturating_sub(EDGES_MAX);
    if excess > 0 {
        st.edges.drain(0..excess);
    }
}

/// The reply's `mentions` block (None when nothing resolved). Call after `commit`.
pub fn report(w: &World, res: &Resolution, plan: &Plan) -> Option<Value> {
    if !res.any() {
        return None;
    }
    let mut notified = vec![];
    let mut not_notified = vec![];
    for (t, fate, reached) in &plan.per_target {
        match fate {
            Fate::Notified | Fate::RodeAlong => notified.push(match t {
                Target::Captain(_) => target_name(w, *t),
                Target::Corp(_) => format!("{} ({reached} member{})", target_name(w, *t), if *reached == 1 { "" } else { "s" }),
            }),
            _ => not_notified.push(json!({"name": target_name(w, *t), "why": fate.name()})),
        }
    }
    let mut v = json!({});
    let resolved = res.mentions.names(w);
    if !resolved.is_empty() {
        v["resolved"] = json!(resolved);
    }
    if !notified.is_empty() {
        v["notified"] = json!(notified);
        v["note"] = json!("A captain may mute your mentions; you are not told.");
    }
    if !not_notified.is_empty() {
        v["not_notified"] = json!(not_notified);
    }
    if res.ignored > 0 {
        v["ignored_inline"] = json!(res.ignored);
    }
    if plan.over_cap || plan.left_after <= 3 {
        let window = w.rules.mention_window_ticks;
        let max = w.rules.mention_notify_max;
        let mut cap = json!({
            "left": plan.left_after,
            "max": max,
            "window_ticks": window,
        });
        // The tick the next unit frees: with `used` units in the window, `left` grows only once
        // `used - max + 1` of them have expired (more than one when the limit was lowered below
        // what was already spent), so the (used - max)-th oldest names it, or the oldest when
        // used <= max. With a limit of 0, or nothing spent yet, there is no tick to name.
        let mut ticks: Vec<Tick> = w
            .mentions
            .recent
            .get(&plan.from)
            .map(|r| r.iter().copied().filter(|t| in_window(w, *t)).collect())
            .unwrap_or_default();
        ticks.sort_unstable();
        if max > 0
            && let Some(t) = ticks.get(ticks.len().saturating_sub(max as usize))
        {
            cap["frees_at_tick"] = json!(t + window);
        }
        v["cap"] = cap;
    }
    Some(v)
}

/// "2 mentioned, 2 notified": for a write's summary line (no excerpts).
pub fn summary(res: &Resolution, plan: &Plan) -> Option<String> {
    if res.mentions.is_empty() {
        return None;
    }
    let mentioned = res
        .mentions
        .targets()
        .into_iter()
        .filter(|t| *t != Target::Captain(plan.from))
        .count();
    // Naming only yourself mentions no one.
    if mentioned == 0 {
        return None;
    }
    let notified = plan
        .per_target
        .iter()
        .filter(|(_, f, _)| matches!(f, Fate::Notified | Fate::RodeAlong))
        .count();
    Some(format!("{mentioned} mentioned, {notified} notified"))
}

/// Unread notices that mention the captain (mention notices and marked riders).
pub fn unread(p: &Player) -> usize {
    p.inbox
        .iter()
        .filter(|n| !n.read && (n.kind == "mention" || n.mention.is_some()))
        .count()
}

// ---------------------------------------------------------------------------------------------
// Moderation

fn cites(item: Item, c: &crate::chronicle::Citation) -> bool {
    use crate::chronicle::Record;
    match item {
        Item::Radio(id) => c.record == Record::Radio && c.id == Some(id),
        Item::Page(id) => c.record == Record::Page && c.id == Some(id),
        Item::Proposal(id) => c.record == Record::Proposal && c.id == Some(id),
        Item::Article(id) => c.record == Record::Article && c.id == Some(id),
        Item::Log(..) => false,
    }
}

fn strike_citation(c: &mut crate::chronicle::Citation) -> bool {
    if c.on_record == STRUCK {
        return false;
    }
    if c.struck_original.is_none() {
        c.struck_original = Some(std::mem::take(&mut c.on_record));
    }
    c.on_record = STRUCK.to_string();
    true
}

/// Moderation reaches the copies (engine 1.58): every notice quote about `item` and every
/// citation copy of it (Chronicle revisions and talk, identity log lines) reads as struck; the
/// first original is kept for the operator (`struck_quote`, `struck_original`). Returns how many
/// copies changed. Call only after the moderation action's own validation passed.
pub fn scrub_copies(w: &mut World, item: Item) -> usize {
    let mut n = 0;
    for p in w.players.values_mut() {
        for note in p.inbox.iter_mut() {
            if note.about.as_ref().map(Place::item) != Some(item) {
                continue;
            }
            if let Some(q) = note.quote.as_mut()
                && q != STRUCK
            {
                let orig = std::mem::replace(q, STRUCK.to_string());
                if note.struck_quote.is_none() {
                    note.struck_quote = Some(orig);
                }
                n += 1;
            }
        }
    }
    for page in w.chronicle.pages.values_mut() {
        for c in page
            .revisions
            .iter_mut()
            .flat_map(|r| r.citations.iter_mut())
            .chain(page.talk.iter_mut().flat_map(|t| t.citations.iter_mut()))
        {
            if cites(item, c) && strike_citation(c) {
                n += 1;
            }
        }
    }
    for i in w.identities.values_mut() {
        for c in i.log.iter_mut().flat_map(|l| l.citations.iter_mut()) {
            if cites(item, c) && strike_citation(c) {
                n += 1;
            }
        }
    }
    n
}

// ---------------------------------------------------------------------------------------------
// The mentions tool

fn mutes_json(w: &World, m: Option<&Mutes>) -> Value {
    let Some(m) = m else {
        return json!({"captains": [], "corps": []});
    };
    json!({
        "captains": m.captains.iter().map(|c| captain_name(w, *c)).collect::<Vec<_>>(),
        "corps": m.corps.iter().map(|k| w.corps.get(k).map(|c| c.name.clone()).unwrap_or_else(|| format!("corp #{k}"))).collect::<Vec<_>>(),
    })
}

/// `mentions action=list`: the captain's mutes and unread mentions. A read, never journaled.
pub fn read(w: &World, pid: PlayerId, a: &Args) -> Res<Value> {
    let p = w
        .players
        .get(&pid)
        .ok_or_else(|| Refusal::new("unknown_player", "No such captain."))?;
    if a.mutates() {
        return Err(Refusal::new(
            "invalid_args",
            "Only list is a read; mute and unmute change your mutes.",
        ));
    }
    Ok(json!({
        "muted": mutes_json(w, w.mentions.mutes.get(&pid)),
        "unread_mentions": unread(p),
        "read": "read_inbox kind=mention lists them.",
    }))
}

/// A mute whose target has no name any more (a removed captain, a corporation that is gone),
/// named the way the list shows it: "#id" or "corp #id" ("corp:#id" also works).
fn gone_mute(w: &World, current: &Mutes, raw: &str) -> Option<Target> {
    let raw = raw.trim();
    let lower = raw.to_ascii_lowercase();
    let corp = lower.strip_prefix("corp").map(|r| r.trim_start_matches([':', ' ']));
    let (rest, is_corp) = match corp {
        Some(r) => (r, true),
        None => (lower.as_str(), false),
    };
    let id: u32 = rest.strip_prefix('#')?.trim().parse().ok()?;
    if is_corp {
        (current.corps.contains(&id) && !w.corps.contains_key(&id)).then_some(Target::Corp(id))
    } else {
        (current.captains.contains(&id) && !w.players.contains_key(&id)).then_some(Target::Captain(id))
    }
}

/// `mentions action=mute|unmute` (and list). Works during an intermission and while muted on
/// radio; the sender is never told.
pub fn write(ctx: &mut Ctx, pid: PlayerId, a: &Args) -> Res<Outcome> {
    ctx.player(pid)?;
    if !a.mutates() {
        let reply = read(ctx.w, pid, a)?;
        return Ok(Outcome {
            summary: "Your mutes.".into(),
            reply,
            events: vec![],
        });
    }
    let raw = a
        .target
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| Refusal::new("invalid_args", "mute and unmute need target: \"Call Sign\" or \"corp:Name\"."))?;
    let current = ctx.w.mentions.mutes.get(&pid).cloned().unwrap_or_default();
    let mute = matches!(a.action, Action::Mute);
    // A muted captain that was removed, or a corporation that is gone, no longer has a name:
    // unmute takes the form the list shows ("#id", "corp #id").
    let gone = if mute { None } else { gone_mute(ctx.w, &current, raw) };
    let t = match gone {
        Some(t) => t,
        None => entry(ctx.w, &index(ctx.w), raw)?,
    };
    if t == Target::Captain(pid) {
        return Err(Refusal::new("invalid_target", "You cannot mute yourself."));
    }
    let name = match t {
        Target::Corp(k) if !ctx.w.corps.contains_key(&k) => format!("corp #{k}"),
        _ => target_name(ctx.w, t),
    };
    let has = match t {
        Target::Captain(c) => current.captains.contains(&c),
        Target::Corp(k) => current.corps.contains(&k),
    };
    if mute && !has && current.len() >= MUTES_MAX {
        return Err(Refusal::new(
            "too_many_mutes",
            format!("You already mute {MUTES_MAX} captains and corporations. Nothing was changed."),
        )
        .hint("mentions action=unmute frees one."));
    }
    let changed = mute != has;
    if changed {
        let m = ctx.w.mentions.mutes.entry(pid).or_default();
        match (t, mute) {
            (Target::Captain(c), true) => {
                m.captains.insert(c);
            }
            (Target::Captain(c), false) => {
                m.captains.remove(&c);
            }
            (Target::Corp(k), true) => {
                m.corps.insert(k);
            }
            (Target::Corp(k), false) => {
                m.corps.remove(&k);
            }
        }
        if m.is_empty() {
            ctx.w.mentions.mutes.remove(&pid);
        }
    }
    let key = if mute { "muted" } else { "unmuted" };
    let summary = match (mute, changed) {
        (true, true) => format!("Muted mentions from {name}."),
        (true, false) => format!("Mentions from {name} were already muted."),
        (false, true) => format!("Unmuted mentions from {name}."),
        (false, false) => format!("Mentions from {name} were not muted."),
    };
    Ok(Outcome {
        summary,
        reply: json!({key: name, "changed": changed}),
        events: vec![],
    })
}

/// The mention rules as `game_rules` lists them.
pub fn rules_view(w: &World) -> Value {
    json!({
        "notify_max": w.rules.mention_notify_max,
        "window_ticks": w.rules.mention_window_ticks,
        "targets_per_item": TARGETS_MAX,
        "surfaces": Surface::ALL.iter().map(|s| s.name()).collect::<Vec<_>>(),
        "mute": "mentions action=mute",
    })
}

// ---------------------------------------------------------------------------------------------
// The operator's view

/// Which seasons the operator's mention view covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeasonSel {
    Current,
    All,
    Pick { season: u32, epoch: u32 },
}

fn place_json(p: &Place) -> Value {
    serde_json::to_value(p).unwrap_or(Value::Null)
}

fn target_kind(t: Target) -> (&'static str, u32) {
    match t {
        Target::Captain(c) => ("captain", c),
        Target::Corp(k) => ("corp", k),
    }
}

fn plain_name(w: &World, t: Target) -> String {
    match t {
        Target::Captain(c) => captain_name(w, c),
        Target::Corp(k) => corp_name(w, k, None),
    }
}

/// Who mentions whom (operator only): pairs per season from the graph, nodes with in, out and
/// mutual counts, the newest edges and every mute. Filters: a captain (as sender or target) and
/// a corporation (as target, or the sender's current corporation).
pub fn operator_view(
    w: &World,
    season: SeasonSel,
    captain: Option<PlayerId>,
    corp: Option<CorpId>,
    recent: usize,
    since: Option<Tick>,
) -> Value {
    let st = &w.mentions;
    let (cur_s, cur_e) = (w.season.number, w.archive.epoch);
    let picks = |e: u32, s: u32| match season {
        SeasonSel::Current => (s, e) == (cur_s, cur_e),
        SeasonSel::All => true,
        SeasonSel::Pick { season, epoch } => (s, e) == (season, epoch),
    };
    let keep = |from: PlayerId, to: Target| {
        captain.is_none_or(|c| from == c || to == Target::Captain(c))
            && corp.is_none_or(|k| to == Target::Corp(k) || w.players.get(&from).is_some_and(|p| p.corp == Some(k)))
    };
    let mut seasons: BTreeSet<(u32, u32)> = st.graph.iter().map(|r| (r.epoch, r.season)).collect();
    seasons.insert((cur_e, cur_s));
    // Pairs, summed across the selected seasons.
    struct Sum {
        by_surface: [u64; SURFACES],
        count: u64,
        notified: u64,
        muted: u64,
        last_tick: Tick,
    }
    let mut pairs: BTreeMap<(PlayerId, Target), Sum> = BTreeMap::new();
    let mut rows = 0usize;
    for r in st.graph.iter().filter(|r| picks(r.epoch, r.season)) {
        rows += 1;
        if !keep(r.from, r.to) {
            continue;
        }
        let s = pairs.entry((r.from, r.to)).or_insert(Sum {
            by_surface: [0; SURFACES],
            count: 0,
            notified: 0,
            muted: 0,
            last_tick: 0,
        });
        for (a, b) in s.by_surface.iter_mut().zip(r.by_surface.iter()) {
            *a += b;
        }
        s.count += r.count;
        s.notified += r.notified;
        s.muted += r.muted;
        s.last_tick = s.last_tick.max(r.last_tick);
    }
    let overflow: u64 = st.overflow.iter().filter(|o| picks(o.epoch, o.season)).map(|o| o.count).sum();
    let mutual = |from: PlayerId, to: Target| match to {
        Target::Captain(c) => pairs.contains_key(&(c, Target::Captain(from))),
        Target::Corp(_) => false,
    };
    let mut pair_rows: Vec<(u64, String, String, Value)> = pairs
        .iter()
        .map(|((from, to), s)| {
            let (kind, id) = target_kind(*to);
            let surfaces: serde_json::Map<String, Value> = Surface::ALL
                .iter()
                .filter(|x| s.by_surface[x.index()] > 0)
                .map(|x| (x.name().to_string(), json!(s.by_surface[x.index()])))
                .collect();
            let (fname, tname) = (captain_name(w, *from), plain_name(w, *to));
            let v = json!({
                "from": fname, "from_id": from, "to": tname, "to_kind": kind, "to_id": id,
                "count": s.count, "notified": s.notified, "muted": s.muted,
                "surfaces": surfaces, "last_tick": s.last_tick, "mutual": mutual(*from, *to),
            });
            (s.count, fname, tname, v)
        })
        .collect();
    pair_rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.cmp(&b.2)));
    // Nodes.
    let mut nodes: BTreeMap<Target, (u64, u64, u64)> = BTreeMap::new();
    for ((from, to), s) in &pairs {
        nodes.entry(Target::Captain(*from)).or_default().1 += s.count;
        nodes.entry(*to).or_default().0 += s.count;
        if mutual(*from, *to) {
            nodes.entry(Target::Captain(*from)).or_default().2 += 1;
        }
    }
    let mut node_rows: Vec<Value> = nodes
        .iter()
        .map(|(t, (inn, out, mutual))| {
            let (kind, id) = target_kind(*t);
            json!({"kind": kind, "id": id, "name": plain_name(w, *t), "out": out, "in": inn, "mutual": mutual})
        })
        .collect();
    node_rows.sort_by(|a, b| {
        let total = |v: &Value| v["in"].as_u64().unwrap_or(0) + v["out"].as_u64().unwrap_or(0);
        total(b).cmp(&total(a)).then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
    });
    let totals = json!({
        "mentions": pairs.values().map(|s| s.count).sum::<u64>(),
        "notified": pairs.values().map(|s| s.notified).sum::<u64>(),
        "muted": pairs.values().map(|s| s.muted).sum::<u64>(),
        "captains_mentioning": pairs.keys().map(|(f, _)| *f).collect::<BTreeSet<_>>().len(),
        "targets": pairs.keys().map(|(_, t)| *t).collect::<BTreeSet<_>>().len(),
        "overflow": overflow, "rows": rows, "rows_max": GRAPH_ROWS_PER_SEASON,
    });
    let recent_rows: Vec<Value> = st
        .edges
        .iter()
        .rev()
        .filter(|e| picks(e.epoch, e.season) && keep(e.from, e.to) && since.is_none_or(|s| e.tick >= s))
        .take(recent)
        .map(|e| {
            let (kind, id) = target_kind(e.to);
            json!({
                "tick": e.tick, "at_ms": e.at_ms, "from": captain_name(w, e.from), "from_id": e.from,
                "to": plain_name(w, e.to), "to_kind": kind, "to_id": id, "fate": e.fate,
                "delivered": e.delivered, "muted": e.muted, "at": place_json(&e.at),
            })
        })
        .collect();
    let mut mute_pairs = vec![];
    for (by, m) in &st.mutes {
        let targets = m
            .captains
            .iter()
            .map(|c| Target::Captain(*c))
            .chain(m.corps.iter().map(|k| Target::Corp(*k)));
        for t in targets {
            if captain.is_some_and(|c| *by != c && t != Target::Captain(c)) || corp.is_some_and(|k| t != Target::Corp(k)) {
                continue;
            }
            let (kind, id) = target_kind(t);
            mute_pairs.push(json!({"by": captain_name(w, *by), "by_id": by, "muted": plain_name(w, t), "kind": kind, "id": id}));
        }
    }
    json!({
        "season": match season {
            SeasonSel::Current => json!({"season": cur_s, "epoch": cur_e}),
            SeasonSel::All => json!("all"),
            SeasonSel::Pick { season, epoch } => json!({"season": season, "epoch": epoch}),
        },
        "seasons": seasons.iter().rev().map(|(e, s)| json!({"season": s, "epoch": e})).collect::<Vec<_>>(),
        "totals": totals,
        "nodes": node_rows,
        "pairs": pair_rows.into_iter().map(|p| p.3).collect::<Vec<_>>(),
        "recent": recent_rows,
        "mutes": {
            "captains_muting": st.mutes.values().filter(|m| !m.is_empty()).count(),
            "pairs": mute_pairs,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::galaxy::{GenesisConfig, generate};
    use crate::model::{Corp, Notice};

    fn world() -> World {
        generate(&GenesisConfig::new(5, 200))
    }

    fn add_captain(w: &mut World, id: PlayerId, name: &str) {
        let mut p = w.players.values().next().unwrap().clone();
        p.id = id;
        p.name = name.into();
        p.kind = PlayerKind::Agent;
        p.npc = None;
        p.corp = None;
        p.inbox = vec![];
        p.next_notice = 1;
        w.players.insert(id, p);
    }

    fn add_corp(w: &mut World, id: CorpId, name: &str, members: &[PlayerId]) {
        w.corps.insert(
            id,
            Corp {
                id,
                name: name.into(),
                ceo: members[0],
                members: members.to_vec(),
                invites: vec![],
                treasury: 0,
                motto: String::new(),
                created_tick: 0,
                deposits: Default::default(),
                record: Default::default(),
                gov: Default::default(),
                seat: None,
                seat_set_tick: None,
            },
        );
        for m in members {
            w.players.get_mut(m).unwrap().corp = Some(id);
        }
    }

    fn sample() -> World {
        let mut w = world();
        add_captain(&mut w, 101, "Vex");
        add_captain(&mut w, 102, "Vex Arden");
        add_captain(&mut w, 103, "Quiet Comet");
        add_captain(&mut w, 104, "Vexate");
        add_captain(&mut w, 105, "Ora Kell");
        add_corp(&mut w, 7, "Iron Tide", &[105, 103]);
        w
    }

    fn targets(text: &str, w: &World) -> Vec<Target> {
        inline(text, &index(w)).into_iter().map(|(t, _)| t).collect()
    }

    #[test]
    fn word_keys_join_words_and_never_bridge_them() {
        assert_eq!(word_key("Quiet-Comet"), "quiet comet");
        assert_eq!(word_key("quiet comet"), "quiet comet");
        assert_eq!(word_key("Quiet_Comet"), "quiet comet");
        assert_eq!(word_key("Vexate"), "vexate");
        assert_eq!(word_key("Vex ate"), "vex ate");
        assert_eq!(word_key("  -x- "), "x");
    }

    #[test]
    fn inline_mentions_take_the_longest_whole_name() {
        let w = sample();
        let c = Target::Captain;
        assert_eq!(targets("hello @Vex Arden, welcome", &w), vec![c(102)]);
        assert_eq!(targets("@Vex said", &w), vec![c(101)]);
        assert_eq!(targets("@QuietComets ahoy", &w), vec![], "no partial word");
        assert_eq!(targets("@QuietComet", &w), vec![], "camel case is one word");
        assert_eq!(targets("@Quiet Comet's ship", &w), vec![c(103)], "possessive");
        assert_eq!(targets("@quiet-comet!", &w), vec![c(103)], "punctuation and case");
        assert_eq!(targets("@Vex ate the cake", &w), vec![c(101)], "never Vexate");
        assert_eq!(targets("@Vexate", &w), vec![c(104)]);
        assert_eq!(targets("mail a@Vex.c", &w), vec![], "an @ inside a word is no mention");
        assert_eq!(targets("@12 and @P12", &w), vec![], "ids are not names");
        assert_eq!(targets("@nobody here", &w), vec![]);
        // Multibyte text around a match; offsets are chars.
        let found = inline("é日本 @Ora Kell é", &index(&w));
        assert_eq!(found, vec![(c(105), 4)]);
        assert_eq!(targets("日本@Vex", &w), vec![], "after an alphanumeric letter");
        assert_eq!(targets("(@Vex)", &w), vec![c(101)]);
        // The name must start right after the @ (or corp:).
        assert_eq!(targets("meet @ Vex at the dock", &w), vec![], "a space after @");
        assert_eq!(targets("@-Vex @'Vex", &w), vec![], "punctuation after @");
        assert_eq!(targets("@corp: Iron Tide", &w), vec![], "a space after corp:");
    }

    #[test]
    fn corporations_ranks_and_former_names() {
        let mut w = sample();
        // A corporation that shares a captain's name: the captain wins, corp: forces it.
        add_corp(&mut w, 8, "Vexate", &[101]);
        let ix = index(&w);
        assert_eq!(targets("@Vexate", &w), vec![Target::Captain(104)]);
        assert_eq!(targets("@corp:Vexate", &w), vec![Target::Corp(8)]);
        assert_eq!(targets("@CORP:iron tide", &w), vec![Target::Corp(7)]);
        assert_eq!(targets("@Iron Tide", &w), vec![Target::Corp(7)]);
        assert_eq!(targets("@corp:Vex Arden", &w), vec![], "corp: never matches a captain");
        // Former names resolve; a current call sign beats another captain's former one.
        w.identities.entry(101).or_default().former_names.push("Old Salt".into());
        w.identities.entry(105).or_default().former_names.push("Quiet Comet".into());
        assert_eq!(targets("@Old Salt", &w), vec![Target::Captain(101)]);
        assert_eq!(targets("@Quiet Comet", &w), vec![Target::Captain(103)]);
        assert_eq!(entry(&w, &index(&w), "old salt"), Ok(Target::Captain(101)));
        // NPCs and the operator are not names.
        let npc = w.players.values().find(|p| p.npc.is_some()).unwrap().name.clone();
        assert!(targets(&format!("@{npc}"), &w).is_empty());
        let _ = ix;
    }

    #[test]
    fn bare_names_in_news_take_the_longest_match() {
        let w = sample();
        let ix = index(&w);
        let got = names_in("Vex Arden won the daily race", &ix);
        assert_eq!(got, [Target::Captain(102)].into_iter().collect());
        let got = names_in("Vex beat Ora Kell; Vexate watched", &ix);
        assert_eq!(
            got,
            [Target::Captain(101), Target::Captain(104), Target::Captain(105)]
                .into_iter()
                .collect()
        );
        assert!(names_in("Vexing news", &ix).is_empty());
    }

    #[test]
    fn structured_entries_are_exact_and_refusals_suggest_names() {
        let w = sample();
        let ix = index(&w);
        assert_eq!(entry(&w, &ix, "@vex-arden"), Ok(Target::Captain(102)));
        assert_eq!(entry(&w, &ix, "corp:IronTide"), Ok(Target::Corp(7)));
        let r = entry(&w, &ix, "Vex Ardn").unwrap_err();
        assert_eq!(r.code, "unknown_mention");
        assert_eq!(r.hint.as_deref(), Some("Closest: Vex Arden, Vex."));
        let r2 = entry(&w, &ix, "Vex Ardn").unwrap_err();
        assert_eq!(r, r2, "deterministic");
        let r = entry(&w, &ix, "Zzzzzzzzzzzzz").unwrap_err();
        assert_eq!(r.hint.as_deref(), Some("get_briefing comms.captains lists captains."));
        assert_eq!(entry(&w, &ix, &"x".repeat(65)).unwrap_err().code, "unknown_mention");
        let many: Vec<String> = (0..21).map(|_| "Vex".to_string()).collect();
        assert_eq!(structured(&w, &ix, &many).unwrap_err().code, "too_many_mentions");
        let r = resolve(
            &w,
            Some(&["Vex".into(), "corp:Iron Tide".into(), "vex".into()]),
            &["@Ora Kell and @Vex"],
        )
        .unwrap();
        assert_eq!(r.mentions.captains, vec![101, 105]);
        assert_eq!(
            r.mentions.corps,
            vec![CorpMention {
                id: 7,
                name: "Iron Tide".into()
            }]
        );
        assert_eq!(r.offsets[&Target::Captain(101)], (0, 14));
        assert!(r.structured.contains(&Target::Corp(7)));
    }

    #[test]
    fn inline_targets_beyond_the_limit_are_ignored() {
        let mut w = world();
        let mut text = String::new();
        for i in 0..25u32 {
            add_captain(&mut w, 200 + i, &format!("Pilot {i}"));
            text.push_str(&format!("@Pilot {i} "));
        }
        let r = resolve(&w, None, &[&text]).unwrap();
        assert_eq!(r.mentions.captains.len(), TARGETS_MAX);
        assert_eq!(r.ignored, 5);
    }

    #[test]
    fn excerpts_are_char_safe_and_use_ascii_dots() {
        let text = format!("{}@Vex{}", "日".repeat(100), "é\n".repeat(200));
        let e = excerpt(&text, 100);
        assert!(e.starts_with("...") && e.ends_with("..."));
        assert_eq!(e.chars().count(), EXCERPT_CHARS + 6);
        assert!(!e.contains('\n'));
        assert!(e.contains("@Vex"));
        assert!(!e.contains('\u{2026}'));
        assert_eq!(excerpt("short @Vex", 6), "short @Vex");
    }

    #[test]
    fn plans_ride_along_cap_and_mute() {
        let mut w = sample();
        add_captain(&mut w, 106, "Tam Rook");
        w.players.get_mut(&106).unwrap().corp = Some(7);
        w.corps.get_mut(&7).unwrap().members.push(106);
        let res = resolve(&w, None, &["@Ora Kell @corp:Iron Tide @Vex @Vex Arden"]).unwrap();
        let mut others = BTreeMap::new();
        others.insert(
            102,
            Pending {
                kind: "chronicle",
                text: "Page news.".into(),
                quote: None,
                about: None,
                fold: None,
            },
        );
        w.mentions.mutes.entry(106).or_default().captains.insert(101);
        let p = plan(&w, 101, &res, Readers::Everyone, &Mentions::default(), &others, &["x"]);
        // Self (Vex) skipped; Ora direct; Iron Tide reaches 103 and 106 (106 muted, still counted).
        assert_eq!(
            p.per_target,
            vec![
                (Target::Captain(105), Fate::Notified, 1),
                (Target::Captain(102), Fate::RodeAlong, 1),
                (Target::Corp(7), Fate::Notified, 2),
            ]
        );
        assert_eq!(p.units, 2);
        assert_eq!(p.recipients[&106], (Target::Corp(7), false));
        assert_eq!(p.recipients[&105], (Target::Captain(105), true));
        // The limit.
        w.rules.mention_notify_max = 1;
        let p = plan(&w, 101, &res, Readers::Everyone, &Mentions::default(), &others, &["x"]);
        assert!(p.over_cap);
        assert_eq!(p.per_target[2], (Target::Corp(7), Fate::OverCap, 0));
        assert_eq!(p.per_target[1].1, Fate::RodeAlong, "riders are free");
        // Corporation radio: outsiders cannot read it.
        let p = plan(
            &w,
            101,
            &res,
            Readers::CorpMembers(9),
            &Mentions::default(),
            &BTreeMap::new(),
            &["x"],
        );
        assert!(p.per_target.iter().all(|(_, f, _)| *f == Fate::CannotRead));
        // A previous revision's mentions are not told again.
        let prior = res.mentions.clone();
        let p = plan(&w, 101, &res, Readers::Everyone, &prior, &BTreeMap::new(), &["x"]);
        assert!(p.per_target.iter().all(|(_, f, _)| *f == Fate::Repeat));
    }

    #[test]
    fn the_limit_counts_units_frees_them_and_reports_when() {
        let mut w = sample();
        w.rules.mention_notify_max = 2;
        w.rules.mention_window_ticks = 10;
        let start = w.tick;
        let mut ctx = Ctx {
            w: &mut w,
            rng: crate::rng::Rng::new(1),
            events: vec![],
        };
        fn write(ctx: &mut Ctx, id: u64) -> (Fate, Value) {
            let text = "@Ora Kell";
            let res = resolve(ctx.w, None, &[text]).unwrap();
            let p = plan(ctx.w, 101, &res, Readers::Everyone, &Mentions::default(), &BTreeMap::new(), &[text]);
            let place = Place::Radio { id, corp: None };
            send(ctx, 101, "Vex", &p, place.clone(), BTreeMap::new());
            commit(ctx, 101, &p, &place);
            (p.per_target[0].1, report(ctx.w, &res, &p).unwrap())
        }
        let (f, r) = write(&mut ctx, 1);
        assert_eq!(f, Fate::Notified);
        assert_eq!(r["cap"]["left"], 1);
        assert_eq!(r["cap"]["frees_at_tick"], start + 10);
        ctx.w.tick += 3;
        let (f, r) = write(&mut ctx, 2);
        assert_eq!(f, Fate::Notified);
        assert_eq!(r["cap"]["left"], 0);
        assert_eq!(r["cap"]["frees_at_tick"], start + 10, "the oldest unit frees first");
        let (f, r) = write(&mut ctx, 3);
        assert_eq!(f, Fate::OverCap);
        assert_eq!(r["not_notified"][0]["why"], "over_cap");
        assert_eq!(ctx.w.mentions.recent[&101].len(), 2, "an over-limit write spends nothing");
        // The window slides: at start + 10 the first unit is free again.
        ctx.w.tick = start + 10;
        let (f, r) = write(&mut ctx, 4);
        assert_eq!(f, Fate::Notified);
        assert_eq!(r["cap"]["frees_at_tick"], start + 13);
        assert_eq!(ctx.w.players[&105].inbox.len(), 3);
        // With a limit of 0 nothing ever frees, so no tick is named.
        ctx.w.rules.mention_notify_max = 0;
        ctx.w.mentions.recent.clear();
        let (f, r) = write(&mut ctx, 5);
        assert_eq!(f, Fate::OverCap);
        assert_eq!(r["cap"]["left"], 0);
        assert!(r["cap"].get("frees_at_tick").is_none(), "{r}");
        assert_eq!(ctx.w.mentions.edges.len(), 5, "every write is recorded, told or not");
        // A limit lowered below what was already spent: 5 units in the window at start+20..24
        // and a limit of 2. Two units must expire before one frees, so the tick named is the
        // fourth oldest's expiry (start + 23 + 10), not the oldest's.
        ctx.w.rules.mention_notify_max = 2;
        ctx.w.tick = start + 29;
        ctx.w.mentions.recent.insert(101, (0..5).map(|i| start + 24 - i).collect());
        let (f, r) = write(&mut ctx, 6);
        assert_eq!(f, Fate::OverCap);
        assert_eq!(r["cap"]["frees_at_tick"], start + 33, "{r}");
        // And at that tick a unit really is free; one tick earlier none is.
        ctx.w.tick = start + 32;
        assert_eq!(left(ctx.w, 101), 0);
        ctx.w.tick = start + 33;
        assert_eq!(left(ctx.w, 101), 1);
    }

    fn row(epoch: u32, season: u32, from: PlayerId, to: Target) -> GraphRow {
        GraphRow {
            epoch,
            season,
            from,
            to,
            by_surface: [1, 0, 0, 0, 0, 0],
            count: 1,
            notified: 1,
            muted: 0,
            first_tick: 5,
            last_tick: 5,
        }
    }

    #[test]
    fn the_graph_stays_sorted_and_overflows_per_season() {
        let mut st = State::default();
        let order = [
            (0, 2, 5, Target::Corp(1)),
            (0, 1, 9, Target::Captain(2)),
            (1, 1, 1, Target::Captain(3)),
            (0, 1, 9, Target::Captain(1)),
        ];
        for (e, s, f, t) in order {
            record(&mut st, e, s, f, t, &row(e, s, f, t));
        }
        record(&mut st, 0, 1, 9, Target::Captain(2), &row(0, 1, 9, Target::Captain(2)));
        let keys: Vec<_> = st.graph.iter().map(row_key).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert_eq!(st.graph.len(), 4);
        assert_eq!(
            st.graph.iter().find(|r| r.from == 9 && r.to == Target::Captain(2)).unwrap().count,
            2
        );
        // Fill one season.
        let mut st = State::default();
        for i in 0..GRAPH_ROWS_PER_SEASON as u32 {
            record(&mut st, 0, 1, i, Target::Captain(1), &row(0, 1, i, Target::Captain(1)));
        }
        record(&mut st, 0, 1, 999_999, Target::Captain(1), &row(0, 1, 0, Target::Captain(1)));
        record(&mut st, 0, 1, 0, Target::Captain(1), &row(0, 1, 0, Target::Captain(1)));
        record(&mut st, 0, 2, 0, Target::Captain(1), &row(0, 2, 0, Target::Captain(1)));
        assert_eq!(st.graph.len(), GRAPH_ROWS_PER_SEASON + 1);
        assert_eq!(
            st.overflow,
            vec![Overflow {
                epoch: 0,
                season: 1,
                count: 1
            }]
        );
        assert_eq!(st.graph[0].count, 2, "existing rows still count");
    }

    #[test]
    fn the_public_index_keeps_the_newest_places() {
        let mut st = State::default();
        for i in 0..(INDEX_KEEP as u64 + 10) {
            index_place(&mut st, Target::Captain(4), &Place::Radio { id: i, corp: None });
        }
        let v = &st.by_captain[&4];
        assert_eq!(v.len(), INDEX_KEEP);
        assert_eq!(v[0], Place::Radio { id: 10, corp: None });
        assert_eq!(
            v.last(),
            Some(&Place::Radio {
                id: INDEX_KEEP as u64 + 9,
                corp: None
            })
        );
    }

    #[test]
    fn scrubbing_copies_keeps_the_first_original_once() {
        use crate::chronicle::{Citation, Record};
        let mut w = sample();
        let note = |id: u64, about: Option<Place>, quote: &str| Notice {
            id,
            tick: 0,
            at_ms: 0,
            kind: "mention".into(),
            text: "server".into(),
            read: false,
            quote: Some(quote.into()),
            about,
            mention: None,
            fold: None,
            superseded_by: None,
            struck_quote: None,
        };
        let inbox = &mut w.players.get_mut(&105).unwrap().inbox;
        inbox.push(note(1, Some(Place::Talk { page: 3, comment: 9 }), "rude words"));
        inbox.push(note(2, Some(Place::Radio { id: 3, corp: None }), "radio words"));
        inbox.push(note(3, None, "no locator"));
        let cite = |record, id, on: &str| Citation {
            record,
            id: Some(id),
            tick: None,
            note: String::new(),
            on_record: on.into(),
            struck_original: None,
        };
        w.identities.entry(105).or_default().log.push(crate::identity::LogEntry {
            citations: vec![cite(Record::Page, 3, "Chronicle page 3: Rude"), cite(Record::Radio, 3, "radio")],
            ..Default::default()
        });
        assert_eq!(scrub_copies(&mut w, Item::Page(3)), 2);
        let p = &w.players[&105];
        assert_eq!(p.inbox[0].quote.as_deref(), Some(STRUCK));
        assert_eq!(p.inbox[0].struck_quote.as_deref(), Some("rude words"));
        assert_eq!(p.inbox[1].quote.as_deref(), Some("radio words"));
        assert_eq!(p.inbox[2].quote.as_deref(), Some("no locator"));
        let c = &w.identities[&105].log[0].citations;
        assert_eq!(c[0].on_record, STRUCK);
        assert_eq!(c[0].struck_original.as_deref(), Some("Chronicle page 3: Rude"));
        assert_eq!(c[1].on_record, "radio");
        // Again: nothing changes and the first original stays.
        assert_eq!(scrub_copies(&mut w, Item::Page(3)), 0);
        assert_eq!(w.players[&105].inbox[0].struck_quote.as_deref(), Some("rude words"));
        assert_eq!(
            w.identities[&105].log[0].citations[0].struck_original.as_deref(),
            Some("Chronicle page 3: Rude")
        );
    }

    #[test]
    fn folds_supersede_unread_notices_with_new_ids() {
        let mut w = sample();
        let mut ctx = Ctx {
            w: &mut w,
            rng: crate::rng::Rng::new(1),
            events: vec![],
        };
        let rev = |r: u32, by: PlayerId| Fold::PageRevised {
            page: 4,
            from_revision: r,
            to_revision: r,
            by: vec![by],
        };
        let first = ctx
            .notify_fold(105, "chronicle", rev(2, 101), fold_text, Some("Title".into()), None)
            .unwrap();
        let second = ctx.notify_fold(105, "chronicle", rev(3, 102), fold_text, None, None).unwrap();
        let third = ctx.notify_fold(105, "chronicle", rev(4, 101), fold_text, None, None).unwrap();
        assert!(first < second && second < third);
        let inbox = &ctx.w.players[&105].inbox;
        assert_eq!((inbox[0].read, inbox[0].superseded_by), (true, Some(second)));
        assert_eq!((inbox[1].read, inbox[1].superseded_by), (true, Some(third)));
        let last = &inbox[2];
        assert!(!last.read);
        assert_eq!(
            last.fold,
            Some(Fold::PageRevised {
                page: 4,
                from_revision: 2,
                to_revision: 4,
                by: vec![101, 102]
            })
        );
        assert_eq!(
            last.text,
            "Chronicle page 4, which you wrote or endorsed, has new revisions 2 to 4 by Vex and Vex Arden. chronicle action=read page=4."
        );
        // Once read, the next event starts a new range; another key never merges.
        ctx.w.players.get_mut(&105).unwrap().inbox[2].read = true;
        ctx.notify_fold(105, "chronicle", rev(5, 101), fold_text, None, None).unwrap();
        ctx.notify_fold(
            105,
            "chronicle",
            Fold::Talk {
                page: 4,
                from_comment: 9,
                to_comment: 9,
                comments: 1,
                by: vec![101],
            },
            fold_text,
            None,
            None,
        )
        .unwrap();
        let inbox = &ctx.w.players[&105].inbox;
        assert_eq!(
            inbox[3].text,
            "Chronicle page 4, which you wrote or endorsed, has a new revision 5 by Vex. chronicle action=read page=4."
        );
        assert!(!inbox[3].read && !inbox[4].read);
        assert_eq!(
            inbox[4].text,
            "Chronicle page 4, which you wrote, has 1 new comment by Vex. chronicle action=read page=4."
        );
        // NPCs get nothing.
        let npc = *ctx.w.players.iter().find(|(_, p)| p.npc.is_some()).unwrap().0;
        assert_eq!(ctx.notify_fold(npc, "chronicle", rev(2, 101), fold_text, None, None), None);
    }

    #[test]
    fn mute_list_and_operator_view() {
        let mut w = sample();
        let mut ctx = Ctx {
            w: &mut w,
            rng: crate::rng::Rng::new(1),
            events: vec![],
        };
        let mute = |t: &str| Args {
            action: Action::Mute,
            target: Some(t.into()),
        };
        assert_eq!(
            write(&mut ctx, 105, &mute("Vex")).unwrap().reply,
            json!({"muted": "Vex", "changed": true})
        );
        assert_eq!(write(&mut ctx, 105, &mute("vex")).unwrap().reply["changed"], false);
        assert_eq!(write(&mut ctx, 105, &mute("Ora Kell")).unwrap_err().code, "invalid_target");
        assert_eq!(write(&mut ctx, 105, &mute("Nobody At All")).unwrap_err().code, "unknown_mention");
        write(&mut ctx, 105, &mute("corp:Iron Tide")).unwrap();
        let list = read(
            ctx.w,
            105,
            &Args {
                action: Action::List,
                target: None,
            },
        )
        .unwrap();
        assert_eq!(list["muted"], json!({"captains": ["Vex"], "corps": ["Iron Tide"]}));
        let un = Args {
            action: Action::Unmute,
            target: Some("Vex".into()),
        };
        assert_eq!(write(&mut ctx, 105, &un).unwrap().reply, json!({"unmuted": "Vex", "changed": true}));
        // A write: plan, send, commit, report. Quiet Comet mutes the sender.
        ctx.w.mentions.mutes.entry(103).or_default().captains.insert(102);
        let res = resolve(ctx.w, None, &["hi @Ora Kell and @corp:Iron Tide"]).unwrap();
        let p = plan(
            ctx.w,
            102,
            &res,
            Readers::Everyone,
            &Mentions::default(),
            &BTreeMap::new(),
            &["hi @Ora Kell and @corp:Iron Tide"],
        );
        let place = Place::Radio { id: 77, corp: None };
        send(&mut ctx, 102, "Vex Arden", &p, place.clone(), BTreeMap::new());
        commit(&mut ctx, 102, &p, &place);
        let ora = &ctx.w.players[&105];
        let n = ora.inbox.last().unwrap();
        assert_eq!(n.kind, "mention");
        assert_eq!(
            n.text,
            "Vex Arden mentioned you on public radio (message 77). read_inbox kind=mention shows the excerpt."
        );
        assert_eq!(n.quote.as_deref(), Some("hi @Ora Kell and @corp:Iron Tide"));
        assert_eq!(ora.inbox.len(), 1, "one notice although the corporation was named too");
        assert!(ctx.w.players[&103].inbox.is_empty(), "muted");
        assert_eq!(unread(&ctx.w.players[&105]), 1);
        let rep = report(ctx.w, &res, &p).unwrap();
        assert_eq!(rep["notified"], json!(["Ora Kell", "corp:Iron Tide (1 member)"]));
        assert!(rep.get("cap").is_none());
        let v = operator_view(ctx.w, SeasonSel::Current, None, None, 10, None);
        assert_eq!(v["totals"]["mentions"], 2);
        assert_eq!(v["totals"]["muted"], 1);
        assert_eq!(v["recent"].as_array().unwrap().len(), 2);
        assert_eq!(v["mutes"]["captains_muting"], 2);
        assert_eq!(ctx.w.mentions.by_captain[&105], vec![place.clone()]);
        let v = operator_view(ctx.w, SeasonSel::All, Some(103), None, 10, None);
        assert_eq!(v["totals"]["mentions"], 0);
    }
}
