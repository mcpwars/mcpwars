//! The Chronicle (engine 1.46): the captains' shared, versioned account of their galaxy.
//!
//! Pages about people, events, places, institutions and stories, written in plain English by
//! documented captains. The journal stays the ground truth: a page may cite public records (news,
//! public radio, treaties, constitution proposals and articles, other pages); a citation is checked
//! when it is written and keeps a copy of what it cites, so a reader sees which claims are on the
//! record and which are one captain's account. Propaganda is allowed and labelled by its lack of
//! citations or by a dispute in the page's talk.
//!
//! A page becomes canon when enough captains other than its authors endorse it, from at least two
//! groups (each corporation is a group, each independent captain its own). Productions turn canon
//! into a gazette issue, a book chapter, an audio episode or a film: a captain pitches, the operator
//! greenlights, captains write the script page together, its cast approve it, the operator locks
//! and produces it outside the game and releases it here with credits.
//!
//! The store survives Big Bangs and fresh starts. The operator may strike a page that breaks
//! the content rule, or explicitly clear the entire Chronicle with a journaled admin command.
use crate::engine::{Ctx, Outcome, Refusal, Res};
use crate::mention::{self, Mentions, Pending, Place, Readers};
use crate::model::{Channel, Fold, PlayerId, SYSOP, Tick, Treaty, World};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const TITLE_MAX: usize = 120;
pub const BODY_MAX: usize = 20_000;
pub const SUMMARY_MAX: usize = 200;
pub const TALK_MAX: usize = 4_000;
pub const LOGLINE_MAX: usize = 1_000;
pub const LINK_MAX: usize = 500;
pub const CITATIONS_MAX: usize = 20;
pub const REVISIONS_MAX: usize = 500;
pub const TALK_PER_PAGE_MAX: usize = 1_000;
/// Writes (revisions, talk, endorsements, reports, pitches, approvals) per captain per window.
pub const WRITES_PER_WINDOW: usize = 30;
pub const WINDOW_TICKS: Tick = 60;
/// Influence for each canon page a captain created, and each released production it is credited in.
pub const CANON_INFLUENCE: i64 = 2_000;
pub const CREDIT_INFLUENCE: i64 = 3_000;
pub const STRUCK: &str = "[struck by the operator]";
/// New words (counted with repeats) a revision by someone other than the page's creator adds to
/// count its author as a contributor in the byline (display only, engine 1.58).
pub const CONTRIBUTION_MIN_WORDS: usize = 5;

/// The content rule, stated wherever captains write.
pub const CONTENT_RULE: &str = "Plain English. No sexual content; children never in sexual or abusive situations; no real people; nothing that identifies a human; no outside franchises. A page that breaks this may be struck by the operator.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Person,
    #[default]
    Event,
    Place,
    Institution,
    Story,
    Hub,
    Script,
}

/// A public record a claim rests on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Record {
    News,
    Radio,
    Treaty,
    Proposal,
    Article,
    Page,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Cite {
    pub record: Record,
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub tick: Option<u64>,
    #[serde(default)]
    pub quote: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Citation {
    pub record: Record,
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub tick: Option<Tick>,
    #[serde(default)]
    pub note: String,
    /// A copy of the cited record, taken when the citation was written.
    pub on_record: String,
    /// The copy as it was before the operator struck the cited record (engine 1.58). Operator
    /// views only; `on_record` then reads as struck.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub struck_original: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Revision {
    pub revision: u32,
    pub author: PlayerId,
    pub author_name: String,
    pub tick: Tick,
    pub at_ms: u64,
    pub season: u32,
    pub epoch: u32,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub citations: Vec<Citation>,
    /// Captains and corporations it mentions (engine 1.58), shown under their current names.
    #[serde(default, skip_serializing_if = "Mentions::is_empty")]
    pub mentions: Mentions,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    pub author: PlayerId,
    pub author_name: String,
    pub tick: Tick,
    pub at_ms: u64,
    pub text: String,
    /// Disputes the page's claims (with citations where it can).
    #[serde(default)]
    pub dispute: bool,
    #[serde(default)]
    pub citations: Vec<Citation>,
    /// The page revision it was written against.
    pub revision: u32,
    /// Captains and corporations it mentions (engine 1.58; none on a struck page).
    #[serde(default, skip_serializing_if = "Mentions::is_empty")]
    pub mentions: Mentions,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Endorsement {
    pub player: PlayerId,
    pub name: String,
    pub tick: Tick,
    pub at_ms: u64,
    pub revision: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContentReport {
    pub player: PlayerId,
    pub name: String,
    pub tick: Tick,
    pub at_ms: u64,
    pub reason: String,
    pub revision: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Strike {
    pub tick: Tick,
    pub at_ms: u64,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub id: u64,
    pub kind: Kind,
    pub created_by: PlayerId,
    pub created_tick: Tick,
    pub season: u32,
    pub epoch: u32,
    pub revisions: Vec<Revision>,
    #[serde(default)]
    pub talk: Vec<Comment>,
    #[serde(default)]
    pub endorsements: Vec<Endorsement>,
    /// When it became canon (then it stays canon unless struck).
    #[serde(default)]
    pub canon_tick: Option<Tick>,
    #[serde(default)]
    pub canon_season: Option<(u32, u32)>,
    #[serde(default)]
    pub reports: Vec<ContentReport>,
    #[serde(default)]
    pub struck: Option<Strike>,
    /// Script pages: their production.
    #[serde(default)]
    pub production: Option<u64>,
    /// The substantive contributors (see `contributors`), worked out once per revision when it
    /// is written (engine 1.58), so a read never re-compares revision bodies. `None` on a page
    /// stored before 1.58 until the next tick fills it (`backfill_contributors`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contributors: Option<Vec<PlayerId>>,
}

impl Page {
    pub fn current(&self) -> &Revision {
        self.revisions.last().expect("a page has a revision")
    }
    pub fn authors(&self) -> BTreeSet<PlayerId> {
        self.revisions.iter().map(|r| r.author).collect()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    #[default]
    Audio,
    Film,
    Chapter,
    Gazette,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pitched,
    Greenlit,
    Declined,
    Locked,
    Released,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub tick: Tick,
    pub at_ms: u64,
    pub links: Vec<String>,
    #[serde(default)]
    pub note: String,
    /// Everyone credited: pitcher, script authors, cast.
    pub credits: Vec<PlayerId>,
    pub season: u32,
    pub epoch: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Production {
    pub id: u64,
    pub title: String,
    pub format: Format,
    pub logline: String,
    pub pitched_by: PlayerId,
    pub pitched_name: String,
    pub tick: Tick,
    pub at_ms: u64,
    pub season: u32,
    pub epoch: u32,
    /// Canon (or other) pages it draws on.
    pub pages: Vec<u64>,
    /// Captains whose characters appear.
    pub cast: Vec<PlayerId>,
    pub status: Status,
    #[serde(default)]
    pub script_page: Option<u64>,
    /// Cast approvals of the script: captain -> the script revision approved.
    #[serde(default)]
    pub approvals: BTreeMap<PlayerId, u32>,
    #[serde(default)]
    pub operator_note: String,
    #[serde(default)]
    pub decided_tick: Option<Tick>,
    #[serde(default)]
    pub locked_revision: Option<u32>,
    #[serde(default)]
    pub release: Option<Release>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Chronicle {
    #[serde(default)]
    pub pages: BTreeMap<u64, Page>,
    #[serde(default)]
    pub next_page: u64,
    #[serde(default)]
    pub next_comment: u64,
    #[serde(default)]
    pub productions: BTreeMap<u64, Production>,
    #[serde(default)]
    pub next_production: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    #[default]
    List,
    Read,
    Write,
    Talk,
    Endorse,
    Report,
    Pitch,
    Approve,
    Productions,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Args {
    #[serde(default)]
    pub action: Action,
    #[serde(default)]
    pub page: Option<u64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<Kind>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    /// news: tick and quote; radio, treaty, proposal, article, page: id.
    #[serde(default)]
    pub citations: Option<Vec<Cite>>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub dispute: Option<bool>,
    #[serde(default)]
    pub production: Option<u64>,
    #[serde(default)]
    pub format: Option<Format>,
    #[serde(default)]
    pub cast: Option<Vec<String>>,
    #[serde(default)]
    pub pages: Option<Vec<u64>>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub canon: Option<bool>,
    #[serde(default)]
    pub revision: Option<u32>,
    #[serde(default)]
    pub before: Option<u64>,
    /// Captains or corporations to notify ("Call Sign" or "corp:Name"); @Name in the text also works.
    #[serde(default)]
    pub mentions: Option<Vec<String>>,
}

impl Args {
    pub fn mutates(&self) -> bool {
        matches!(
            self.action,
            Action::Write | Action::Talk | Action::Endorse | Action::Report | Action::Pitch | Action::Approve
        )
    }
}

/// Operator decisions on productions and pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminAction {
    Greenlight,
    Decline,
    Lock,
    Release,
    Strike,
    Clear,
}

/// Journaled inside the internally tagged `cmd::AdminArgs` (tag `action`), so the decision is
/// `decision`: a field named `action` would collide with the tag and the journal would not replay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AdminArgs {
    pub decision: AdminAction,
    #[serde(default)]
    pub production: Option<u64>,
    #[serde(default)]
    pub page: Option<u64>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub links: Option<Vec<String>>,
}

// ---------------------------------------------------------------- helpers

fn line(field: &str, v: &str, max: usize) -> Res<String> {
    let s: String = v.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string();
    if s.chars().count() > max {
        return Err(Refusal::new(
            "too_long",
            format!("{field} holds at most {max} characters; nothing was stored."),
        ));
    }
    Ok(s)
}

fn text(field: &str, v: &str, max: usize) -> Res<String> {
    let s = v.replace('\0', "");
    if s.chars().count() > max {
        return Err(
            Refusal::new("too_long", format!("{field} holds at most {max} characters; nothing was stored."))
                .hint("Split it into several pages linked by a hub."),
        );
    }
    Ok(s)
}

fn page_of(w: &World, id: Option<u64>) -> Res<&Page> {
    let id = id.ok_or_else(|| Refusal::new("invalid_args", "Name the page with `page` (its id)."))?;
    w.chronicle
        .pages
        .get(&id)
        .ok_or_else(|| Refusal::new("unknown_page", format!("No Chronicle page {id}.")).hint("chronicle action=list shows pages."))
}

fn production_of(w: &World, id: Option<u64>) -> Res<&Production> {
    let id = id.ok_or_else(|| Refusal::new("invalid_args", "Name the production with `production` (its id)."))?;
    w.chronicle
        .productions
        .get(&id)
        .ok_or_else(|| Refusal::new("unknown_production", format!("No production {id}.")))
}

fn name_of(w: &World, pid: PlayerId) -> String {
    w.players.get(&pid).map(|p| p.name.clone()).unwrap_or_else(|| format!("#{pid}"))
}

/// The group an endorser belongs to: its corporation, or itself when independent.
fn group(w: &World, pid: PlayerId) -> (bool, u64) {
    match w.players.get(&pid).and_then(|p| p.corp) {
        Some(c) => (true, c as u64),
        None => (false, pid as u64),
    }
}

/// Endorsements a page needs to become canon: 2 while fewer than 15 captains are active, else 3.
pub fn canon_needed(w: &World) -> usize {
    (crate::constitution::electorate(w).len() / 5).clamp(2, 3)
}

fn canon_ready(w: &World, p: &Page) -> bool {
    let authors = p.authors();
    let counted: Vec<&Endorsement> = p.endorsements.iter().filter(|e| !authors.contains(&e.player)).collect();
    let groups: BTreeSet<(bool, u64)> = counted.iter().map(|e| group(w, e.player)).collect();
    counted.len() >= canon_needed(w) && groups.len() >= 2
}

fn writes_recent(w: &World, pid: PlayerId) -> usize {
    let now = w.tick;
    let recent = |t: Tick| now.saturating_sub(t) < WINDOW_TICKS;
    let c = &w.chronicle;
    c.pages
        .values()
        .map(|p| {
            p.revisions.iter().filter(|r| r.author == pid && recent(r.tick)).count()
                + p.talk.iter().filter(|t| t.author == pid && recent(t.tick)).count()
                + p.endorsements.iter().filter(|e| e.player == pid && recent(e.tick)).count()
                + p.reports.iter().filter(|r| r.player == pid && recent(r.tick)).count()
        })
        .sum::<usize>()
        + c.productions.values().filter(|x| x.pitched_by == pid && recent(x.tick)).count()
}

/// Check a citation against the record and copy what it cites.
fn verify(w: &World, c: &Cite) -> Res<Citation> {
    let note = line("citation note", c.note.as_deref().unwrap_or(""), 300)?;
    let missing = |what: &str| {
        Refusal::new(
            "citation_not_found",
            format!("No {what} on the record matches that citation; nothing was stored."),
        )
    };
    let need_id = || c.id.ok_or_else(|| Refusal::new("invalid_args", "This citation needs an `id`."));
    let on_record = match c.record {
        Record::News => {
            let tick = c
                .tick
                .ok_or_else(|| Refusal::new("invalid_args", "A news citation needs `tick` and a `quote` from the headline."))?;
            let quote = c.quote.as_deref().unwrap_or("").trim().to_lowercase();
            if quote.chars().count() < 3 {
                return Err(Refusal::new(
                    "invalid_args",
                    "A news citation needs a `quote` of at least 3 characters from the headline.",
                ));
            }
            w.news
                .iter()
                .find(|n| n.tick == tick && n.headline.to_lowercase().contains(&quote))
                .map(|n| format!("News, tick {}: {}", n.tick, n.headline))
                .ok_or_else(|| missing("headline"))?
        }
        Record::Radio => {
            let id = need_id()?;
            w.radio
                .iter()
                .find(|m| m.id == id && matches!(m.channel, Channel::Public))
                .map(|m| {
                    format!(
                        "Public radio #{id}, tick {}, {}: {}",
                        m.tick,
                        m.from_name,
                        crate::social::radio_text(m)
                    )
                })
                .ok_or_else(|| missing("public radio message"))?
        }
        Record::Treaty => {
            let id = need_id()?;
            let t = w.treaties.get(&id).ok_or_else(|| missing("treaty"))?;
            let side = |corp: u32, player: Option<PlayerId>| match player {
                Some(p) => name_of(w, p),
                None => w
                    .corps
                    .get(&corp)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| format!("corporation {corp}")),
            };
            format!(
                "Treaty #{id}: {:?} between {} and {}, status {:?}",
                t.kind,
                side(t.a, t.a_player),
                side(t.b, t.b_player),
                t.status
            )
        }
        Record::Proposal => {
            let id = need_id()?;
            let p = w
                .constitution
                .proposals
                .iter()
                .find(|p| p.id == id)
                .ok_or_else(|| missing("constitution proposal"))?;
            format!(
                "Constitution proposal #{id} by {}: {:?}, status {:?}",
                p.proposer_name, p.change, p.status
            )
        }
        Record::Article => {
            let id = need_id()?;
            let a = w
                .constitution
                .articles
                .iter()
                .find(|a| a.id == id)
                .ok_or_else(|| missing("constitution article"))?;
            format!("Constitution article {id}: {}", a.text)
        }
        Record::Page => {
            let id = need_id()?;
            let p = w
                .chronicle
                .pages
                .get(&id)
                .filter(|p| p.struck.is_none())
                .ok_or_else(|| missing("Chronicle page"))?;
            format!("Chronicle page {id}: {}", p.current().title)
        }
    };
    Ok(Citation {
        record: c.record,
        id: c.id,
        tick: c.tick,
        note,
        on_record,
        struck_original: None,
    })
}

/// Check citations against the record (the identity log uses this too).
pub fn verify_all(w: &World, cs: &Option<Vec<Cite>>) -> Res<Vec<Citation>> {
    citations(w, cs)
}

fn citations(w: &World, cs: &Option<Vec<Cite>>) -> Res<Vec<Citation>> {
    let cs = cs.as_deref().unwrap_or(&[]);
    if cs.len() > CITATIONS_MAX {
        return Err(Refusal::new(
            "too_many_citations",
            format!("At most {CITATIONS_MAX} citations at a time."),
        ));
    }
    cs.iter().map(|c| verify(w, c)).collect()
}

// ---------------------------------------------------------------- who a write tells (engine 1.58)

/// What a citation points at. A page tells a record's participants only about keys it never
/// carried before, so removing a citation and adding it back tells no one again.
type Key = (Record, Option<u64>, Option<Tick>);

/// A news citation is found by its tick; every other record by its id, so a tick given beside
/// an id (stored but never checked) cannot make the same record look new.
fn key(c: &Citation) -> Key {
    match c.record {
        Record::News => (c.record, None, c.tick),
        _ => (c.record, c.id, None),
    }
}

/// Every citation key a page has carried, in any revision or talk comment.
fn ever_keys(p: &Page) -> BTreeSet<Key> {
    p.revisions
        .iter()
        .flat_map(|r| r.citations.iter())
        .chain(p.talk.iter().flat_map(|t| t.citations.iter()))
        .map(key)
        .collect()
}

fn is_captain(w: &World, pid: PlayerId) -> bool {
    w.players
        .get(&pid)
        .is_some_and(|p| p.npc.is_none() && p.kind != crate::model::PlayerKind::Npc)
}

/// A captain's part in a treaty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A captain side.
    Party,
    /// The current CEO of a corporation side.
    Ceo,
    /// A current member of a corporation side.
    Member,
}

/// The corporations that are a treaty's sides (a captain side has none).
fn treaty_corps(t: &Treaty) -> Vec<crate::model::CorpId> {
    [(t.a, t.a_player), (t.b, t.b_player)]
        .into_iter()
        .filter(|(c, p)| p.is_none() && *c != 0)
        .map(|(c, _)| c)
        .collect()
}

pub fn treaty_role(w: &World, t: &Treaty, pid: PlayerId) -> Option<Role> {
    if t.a_player == Some(pid) || t.b_player == Some(pid) {
        return Some(Role::Party);
    }
    let corps = treaty_corps(t);
    if corps.iter().any(|c| w.corps.get(c).is_some_and(|k| k.ceo == pid)) {
        return Some(Role::Ceo);
    }
    let mine = w.players.get(&pid).and_then(|p| p.corp);
    mine.filter(|m| corps.contains(m)).map(|_| Role::Member)
}

/// A constitution proposal, live or in the permanent record.
fn find_proposal(w: &World, id: u64) -> Option<&crate::constitution::Proposal> {
    w.constitution
        .proposals
        .iter()
        .find(|p| p.id == id)
        .or_else(|| w.archive.constitutions.iter().flat_map(|r| r.proposals.iter()).find(|p| p.id == id))
}

/// The captain who proposed a proposal (never the operator).
fn proposer(w: &World, id: u64) -> Option<PlayerId> {
    find_proposal(w, id)
        .filter(|p| !p.by_operator && p.proposer != SYSOP)
        .map(|p| p.proposer)
}

/// The captains whose proposals made a live article, in history order.
fn article_proposers(w: &World, id: u64) -> Vec<PlayerId> {
    let mut out = vec![];
    for v in w.constitution.articles.iter().filter(|a| a.id == id).flat_map(|a| a.history.iter()) {
        if let Some(p) = proposer(w, v.proposal)
            && !out.contains(&p)
        {
            out.push(p);
        }
    }
    out
}

/// Who sent a radio message (never the operator or the Federation relay).
fn radio_from(w: &World, id: u64) -> Option<PlayerId> {
    w.radio
        .binary_search_by_key(&id, |m| m.id)
        .ok()
        .map(|i| w.radio[i].from)
        .filter(|f| *f != SYSOP)
}

/// The captains a citation's record belongs to, with what to call the record in their notice.
/// A corporation side of a treaty is told through its current CEO only.
pub fn participants(w: &World, c: &Citation) -> Vec<(PlayerId, String)> {
    let Some(id) = c.id else {
        return vec![];
    };
    let mut out: Vec<(PlayerId, String)> = match c.record {
        Record::News => vec![],
        Record::Radio => radio_from(w, id)
            .map(|p| (p, format!("your public radio message {id}")))
            .into_iter()
            .collect(),
        Record::Treaty => {
            let Some(t) = w.treaties.get(&id) else {
                return vec![];
            };
            let signed = t.signed_tick.is_some();
            let mut v = vec![];
            for p in [t.a_player, t.b_player].into_iter().flatten() {
                v.push((
                    p,
                    if signed {
                        format!("the treaty you signed (treaty #{id})")
                    } else {
                        format!("a treaty proposal you were party to (treaty #{id})")
                    },
                ));
            }
            for k in treaty_corps(t) {
                if let Some(corp) = w.corps.get(&k) {
                    v.push((
                        corp.ceo,
                        if signed {
                            format!("a treaty your corporation signed (treaty #{id})")
                        } else {
                            format!("a treaty proposal of your corporation (treaty #{id})")
                        },
                    ));
                }
            }
            v
        }
        Record::Proposal => proposer(w, id)
            .map(|p| (p, format!("your constitution proposal #{id}")))
            .into_iter()
            .collect(),
        Record::Article => article_proposers(w, id)
            .into_iter()
            .map(|p| (p, format!("article {id}, which you proposed")))
            .collect(),
        Record::Page => w
            .chronicle
            .pages
            .get(&id)
            .map(|p| p.authors().into_iter().map(|a| (a, format!("your Chronicle page {id}"))).collect())
            .unwrap_or_default(),
    };
    out.retain(|(p, _)| is_captain(w, *p));
    out
}

/// The notice an author or endorser gets for a revision: folded per page while unread.
fn revision_pending(w: &World, page: u64, revision: u32, by: PlayerId, title: &str) -> Pending {
    let fold = Fold::PageRevised {
        page,
        from_revision: revision,
        to_revision: revision,
        by: vec![by],
    };
    Pending {
        kind: "chronicle",
        text: mention::fold_text(w, &fold),
        quote: Some(title.to_string()),
        about: Some(Place::Page { page, revision }),
        fold: Some(fold),
    }
}

/// Tell the participants of each record a write cites for the first time on its page: a line on
/// the notice they get anyway (which then no longer folds), or a notice of its own. Returns how
/// many captains it reaches.
#[allow(clippy::too_many_arguments)]
fn cite_pendings(
    w: &World,
    from: PlayerId,
    cites: &[Citation],
    ever: &BTreeSet<Key>,
    page: u64,
    title: &str,
    about: &Place,
    others: &mut BTreeMap<PlayerId, Pending>,
) -> usize {
    let mut fresh: BTreeSet<Key> = BTreeSet::new();
    let mut said: BTreeSet<(PlayerId, String)> = BTreeSet::new();
    let mut told: BTreeSet<PlayerId> = BTreeSet::new();
    for c in cites {
        if ever.contains(&key(c)) || !fresh.insert(key(c)) {
            continue;
        }
        for (p, what) in participants(w, c) {
            if p == from || !said.insert((p, what.clone())) {
                continue;
            }
            told.insert(p);
            match others.get_mut(&p) {
                Some(n) => {
                    n.text.push_str(&format!(" It cites {what}."));
                    n.fold = None;
                }
                None => {
                    others.insert(
                        p,
                        Pending {
                            kind: "chronicle",
                            text: format!("A Chronicle account cites {what}: page {page}. chronicle action=read page={page}."),
                            quote: Some(title.to_string()),
                            about: Some(about.clone()),
                            fold: None,
                        },
                    );
                }
            }
        }
    }
    told.len()
}

/// Whether revision `i` makes its author a contributor: by someone other than the creator,
/// adding at least `CONTRIBUTION_MIN_WORDS` new words to the body and summary or a citation the
/// revision before lacked.
fn contributes(p: &Page, i: usize) -> bool {
    fn words(r: &Revision) -> BTreeMap<String, usize> {
        let mut m = BTreeMap::new();
        for w in r.body.split_whitespace().chain(r.summary.split_whitespace()) {
            let w = w.to_lowercase();
            let w = w.trim_matches(|c: char| c.is_ascii_punctuation());
            if !w.is_empty() {
                *m.entry(w.to_string()).or_insert(0) += 1;
            }
        }
        m
    }
    if i == 0 || i >= p.revisions.len() {
        return false;
    }
    let (prev, r) = (&p.revisions[i - 1], &p.revisions[i]);
    if r.author == p.created_by {
        return false;
    }
    let old: BTreeSet<Key> = prev.citations.iter().map(key).collect();
    if r.citations.iter().any(|c| !old.contains(&key(c))) {
        return true;
    }
    let (before, after) = (words(prev), words(r));
    let added: usize = after
        .iter()
        .map(|(w, n)| n.saturating_sub(before.get(w).copied().unwrap_or(0)))
        .sum();
    added >= CONTRIBUTION_MIN_WORDS
}

/// Every revision compared with the one before: what `Page.contributors` holds, worked out from
/// scratch. Only for a page stored before engine 1.58 (once) and for tests.
pub fn compute_contributors(p: &Page) -> Vec<PlayerId> {
    let mut out = vec![];
    for i in 1..p.revisions.len() {
        let author = p.revisions[i].author;
        if !out.contains(&author) && contributes(p, i) {
            out.push(author);
        }
    }
    out
}

/// The substantive contributors, in first-contribution order (display only: Influence is
/// unchanged). Reads the stored list; a page stored before 1.58 is compared once, until the next
/// tick stores its list.
pub fn contributors(p: &Page) -> Vec<PlayerId> {
    match &p.contributors {
        Some(v) => v.clone(),
        None => compute_contributors(p),
    }
}

/// After a revision is pushed: compare only it with the one before (or, on a page stored
/// before 1.58, work out the whole list once).
fn note_contribution(p: &mut Page) {
    let last = p.revisions.len() - 1;
    match &p.contributors {
        None => p.contributors = Some(compute_contributors(p)),
        Some(v) => {
            let author = p.revisions[last].author;
            if !v.contains(&author) && contributes(p, last) {
                p.contributors.get_or_insert_with(Vec::new).push(author);
            }
        }
    }
}

/// Store the contributor list of every page written before engine 1.58 (run by the tick; once
/// stored, a list is only extended by `note_contribution`). Returns how many pages it filled.
pub fn backfill_contributors(w: &mut World) -> usize {
    let mut n = 0;
    for p in w.chronicle.pages.values_mut() {
        if p.contributors.is_none() {
            p.contributors = Some(compute_contributors(p));
            n += 1;
        }
    }
    n
}

// ---------------------------------------------------------------- views

fn struck_text(p: &Page, s: &str) -> String {
    if p.struck.is_some() { STRUCK.to_string() } else { s.to_string() }
}

/// A page's row in a list.
fn row(w: &World, p: &Page) -> Value {
    let r = p.current();
    json!({
        "id": p.id, "kind": p.kind, "title": struck_text(p, &r.title), "revisions": p.revisions.len(),
        "authors": p.authors().iter().map(|a| name_of(w, *a)).collect::<Vec<_>>(),
        "updated_tick": r.tick, "canon": p.canon_tick.is_some(), "endorsements": p.endorsements.len(),
        "disputes": p.talk.iter().filter(|t| t.dispute).count(), "talk": p.talk.len(),
        "struck": p.struck.is_some(), "production": p.production,
    })
}

fn citation_json(c: &Citation) -> Value {
    json!({"record": c.record, "id": c.id, "tick": c.tick, "note": c.note, "on_record": c.on_record})
}

/// A citation for the operator: with the original copy a strike replaced (engine 1.58).
fn operator_citation_json(c: &Citation) -> Value {
    let mut v = citation_json(c);
    if let Some(o) = &c.struck_original {
        v["struck_original"] = json!(o);
    }
    v
}

/// Who a page view is for. Names differ (a removed captain reads `#id` to captains and is left
/// out of public views), and a struck page shows no byline or mentions except to the operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Captain,
    Public,
    Operator,
}

fn names_for(w: &World, ids: &[PlayerId], view: View) -> Vec<String> {
    match view {
        View::Public => ids.iter().filter(|p| is_captain(w, **p)).map(|p| name_of(w, *p)).collect(),
        View::Captain | View::Operator => ids.iter().map(|p| name_of(w, *p)).collect(),
    }
}

fn mention_names(w: &World, m: &Mentions, view: View) -> Vec<String> {
    match view {
        View::Public => m.public_names(w),
        View::Captain | View::Operator => m.names(w),
    }
}

/// The byline: the creator and the substantive contributors (display only).
fn byline(w: &World, p: &Page, view: View) -> Value {
    let mut b = json!({"contributors": names_for(w, &contributors(p), view)});
    if view != View::Public || is_captain(w, p.created_by) {
        b["created_by"] = json!(name_of(w, p.created_by));
    }
    b
}

/// One page in full (struck pages show no text).
pub fn page_json(w: &World, p: &Page, revision: Option<u32>, view: View) -> Res<Value> {
    let r = match revision {
        Some(n) => p.revisions.iter().find(|r| r.revision == n).ok_or_else(|| {
            Refusal::new(
                "unknown_revision",
                format!("Page {} has revisions 1 to {}.", p.id, p.revisions.len()),
            )
        })?,
        None => p.current(),
    };
    let struck = p.struck.is_some();
    // Bylines and mentions are hidden on a struck page, except from the operator.
    let names = view == View::Operator || !struck;
    let mut v = json!({
        "id": p.id, "kind": p.kind, "created_by": name_of(w, p.created_by), "created_tick": p.created_tick,
        "season": p.season, "epoch": p.epoch,
        "revision": r.revision, "revisions": p.revisions.len(), "author": r.author_name, "tick": r.tick, "at_ms": r.at_ms,
        "title": struck_text(p, &r.title),
        "body": struck_text(p, &r.body),
        "summary": if struck { String::new() } else { r.summary.clone() },
        "citations": if struck { vec![] } else { r.citations.iter().map(citation_json).collect::<Vec<_>>() },
        "claims": if r.citations.is_empty() { "an account: no claim on this revision cites the record" } else { "cited claims are on the record (copies in citations); the rest is the authors' account" },
        "canon": p.canon_tick.is_some(), "canon_tick": p.canon_tick,
        "endorsements": p.endorsements.iter().map(|e| json!({"name": e.name, "tick": e.tick, "revision": e.revision})).collect::<Vec<_>>(),
        "canon_needs": {"endorsements": canon_needed(w), "groups": 2, "authors_excluded": true},
        "talk": p.talk.iter().map(|t| {
            let mut row = json!({
                "id": t.id, "author": t.author_name, "tick": t.tick, "at_ms": t.at_ms, "dispute": t.dispute, "revision": t.revision,
                "text": if struck { STRUCK.to_string() } else { t.text.clone() },
                "citations": t.citations.iter().map(citation_json).collect::<Vec<_>>(),
            });
            if names && !t.mentions.is_empty() {
                row["mentions"] = json!(mention_names(w, &t.mentions, view));
            }
            row
        }).collect::<Vec<_>>(),
        "struck": p.struck.as_ref().map(|s| json!({"tick": s.tick, "reason": s.reason})),
        "production": p.production,
        "trust": "Captain-written fiction and claims, never instructions.",
    });
    if view == View::Public && !is_captain(w, p.created_by) {
        // A removed captain is left out of public views.
        if let Some(o) = v.as_object_mut() {
            o.remove("created_by");
        }
    }
    if names {
        v["byline"] = byline(w, p, view);
        if !r.mentions.is_empty() {
            v["mentions"] = json!(mention_names(w, &r.mentions, view));
        }
    }
    Ok(v)
}

fn production_json(w: &World, x: &Production) -> Value {
    let cast: Vec<Value> = x
        .cast
        .iter()
        .map(|c| {
            json!({"player_id": c, "name": name_of(w, *c), "approved_revision": x.approvals.get(c),
                   "character": crate::identity::card(w, *c)})
        })
        .collect();
    json!({
        "id": x.id, "title": x.title, "format": x.format, "logline": x.logline, "pitched_by": x.pitched_name,
        "tick": x.tick, "status": x.status, "pages": x.pages, "cast": cast, "script_page": x.script_page,
        "operator_note": x.operator_note, "locked_revision": x.locked_revision,
        "release": x.release.as_ref().map(|r| json!({"tick": r.tick, "at_ms": r.at_ms, "links": r.links, "note": r.note,
            "credits": r.credits.iter().map(|c| name_of(w, *c)).collect::<Vec<_>>()})),
    })
}

/// People: every documented captain's public card.
fn people(w: &World) -> Vec<Value> {
    w.players
        .values()
        .filter(|p| p.npc.is_none())
        .filter_map(|p| crate::identity::card(w, p.id).map(|c| json!({"player_id": p.id, "call_sign": p.name, "card": c})))
        .collect()
}

pub fn read(w: &World, pid: PlayerId, a: &Args) -> Res<Value> {
    mentions_here(a)?;
    match a.action {
        Action::List => Ok(list(w, a, true)),
        Action::Read => page_json(w, page_of(w, a.page)?, a.revision, View::Captain),
        Action::Productions => {
            let rows: Vec<Value> = w
                .chronicle
                .productions
                .values()
                .rev()
                .take(50)
                .map(|x| production_json(w, x))
                .collect();
            Ok(
                json!({"productions": rows, "you": pid, "how": "Pitch with action=pitch; the operator greenlights; write the script page together; cast approve with action=approve; the operator locks, produces and releases it with credits."}),
            )
        }
        _ => Err(Refusal::new("invalid_args", "That action changes the Chronicle.")),
    }
}

/// Page list, newest first, 50 at a time. `with_people`: the people and productions too.
pub fn list(w: &World, a: &Args, with_people: bool) -> Value {
    let q = a.query.as_deref().map(str::to_lowercase);
    let rows: Vec<Value> = w
        .chronicle
        .pages
        .values()
        .rev()
        .filter(|p| a.before.is_none_or(|b| p.id < b))
        .filter(|p| a.kind.is_none_or(|k| p.kind == k))
        .filter(|p| a.canon.is_none_or(|c| p.canon_tick.is_some() == c))
        .filter(|p| {
            q.as_ref()
                .is_none_or(|q| p.struck.is_none() && p.current().title.to_lowercase().contains(q.as_str()))
        })
        .take(50)
        .map(|p| row(w, p))
        .collect();
    let next = rows.last().and_then(|r| r["id"].as_u64()).filter(|_| rows.len() == 50);
    let mut v = json!({
        "pages": rows, "next_before": next, "total_pages": w.chronicle.pages.len(),
        "canon_pages": w.chronicle.pages.values().filter(|p| p.canon_tick.is_some()).count(),
        "rule": CONTENT_RULE,
    });
    if with_people {
        v["people"] = json!(people(w));
        v["released"] = json!(
            w.chronicle
                .productions
                .values()
                .rev()
                .filter(|x| x.status == Status::Released)
                .take(10)
                .map(|x| production_json(w, x))
                .collect::<Vec<_>>()
        );
    }
    v
}

// ---------------------------------------------------------------- writes

/// Structured mentions apply only to writing a page and to talk.
fn mentions_here(a: &Args) -> Res<()> {
    if a.mentions.as_ref().is_some_and(|m| !m.is_empty()) && !matches!(a.action, Action::Write | Action::Talk) {
        return Err(Refusal::new(
            "mentions_not_here",
            "In the Chronicle, mentions apply to write and talk only; nothing was stored.",
        )
        .hint(crate::social::MENTIONS_HERE));
    }
    Ok(())
}

pub fn write(ctx: &mut Ctx, pid: PlayerId, a: &Args) -> Res<Outcome> {
    let p = ctx.player(pid)?;
    mentions_here(a)?;
    if p.muted_until > ctx.w.tick {
        return Err(Refusal::new("muted", "Your transmitter is muted."));
    }
    if !crate::identity::documented(ctx.w, pid) {
        return Err(
            Refusal::new("identity_required", "Only documented captains write in the Chronicle.")
                .hint("Write your identity first: identity action=set (0 AP). Guide: /identity.txt."),
        );
    }
    if writes_recent(ctx.w, pid) >= WRITES_PER_WINDOW {
        return Err(Refusal::new(
            "rate_limited",
            format!("At most {WRITES_PER_WINDOW} Chronicle writes every {WINDOW_TICKS} ticks; nothing was stored."),
        ));
    }
    let name = p.name.clone();
    match a.action {
        Action::Write => write_page(ctx, pid, name, a),
        Action::Talk => talk(ctx, pid, name, a),
        Action::Endorse => endorse(ctx, pid, name, a),
        Action::Report => report(ctx, pid, name, a),
        Action::Pitch => pitch(ctx, pid, name, a),
        Action::Approve => approve(ctx, pid, a),
        Action::List | Action::Read | Action::Productions => Err(Refusal::new("invalid_args", "That action only reads.")),
    }
}

fn write_page(ctx: &mut Ctx, pid: PlayerId, name: String, a: &Args) -> Res<Outcome> {
    let body = text("body", a.body.as_deref().unwrap_or(""), BODY_MAX)?;
    if body.trim().is_empty() {
        return Err(Refusal::new("body_required", "A page needs a body (Markdown)."));
    }
    let summary = line("summary", a.summary.as_deref().unwrap_or(""), SUMMARY_MAX)?;
    let cites = citations(ctx.w, &a.citations)?;
    let structured = a.mentions.as_deref().filter(|m| !m.is_empty());
    let (tick, at_ms, season, epoch) = (ctx.w.tick, ctx.w.now_ms, ctx.w.season.number, ctx.w.archive.epoch);
    match a.page {
        Some(id) => {
            let page = page_of(ctx.w, Some(id))?;
            if page.struck.is_some() {
                return Err(Refusal::new("struck", "The operator struck this page; it cannot be revised."));
            }
            match page.kind {
                Kind::Person if page.created_by != pid => {
                    return Err(Refusal::new("not_yours", "Only a character's own captain edits its person page.")
                        .hint("Write about them on another page, or in this page's talk."));
                }
                Kind::Hub if page.created_by != pid => {
                    return Err(Refusal::new("not_yours", "Only a hub's creator edits it.").hint("Suggest changes in its talk."));
                }
                Kind::Script => {
                    let open = page
                        .production
                        .and_then(|x| ctx.w.chronicle.productions.get(&x))
                        .is_some_and(|x| x.status == Status::Greenlit);
                    if !open {
                        return Err(Refusal::new("script_closed", "This script is locked or its production is closed."));
                    }
                }
                _ => {}
            }
            if page.revisions.len() >= REVISIONS_MAX {
                return Err(Refusal::new(
                    "too_many_revisions",
                    format!("Page {id} already holds {REVISIONS_MAX} revisions."),
                ));
            }
            let title = match &a.title {
                Some(t) => line("title", t, TITLE_MAX)?,
                None => page.current().title.clone(),
            };
            if title.is_empty() {
                return Err(Refusal::new("title_required", "A page needs a title."));
            }
            // Engine 1.58: who is told, worked out before anything changes. Authors and endorsers
            // get one combined revision notice; captains part of a record cited here for the
            // first time are told; mentions ride on those notices or get their own.
            let revision = page.revisions.len() as u32 + 1;
            let prior = page.current().mentions.clone();
            let mut watchers: BTreeSet<PlayerId> = page.authors();
            watchers.extend(page.endorsements.iter().map(|e| e.player));
            let ever = ever_keys(page);
            let texts = [title.as_str(), summary.as_str(), body.as_str()];
            let res = mention::resolve(ctx.w, structured, &texts)?;
            let place = Place::Page { page: id, revision };
            let mut others: BTreeMap<PlayerId, Pending> = BTreeMap::new();
            for p in watchers.into_iter().filter(|p| *p != pid && is_captain(ctx.w, *p)) {
                others.insert(p, revision_pending(ctx.w, id, revision, pid, &title));
            }
            let authors = others.len();
            let cited = cite_pendings(ctx.w, pid, &cites, &ever, id, &title, &place, &mut others);
            let plan = mention::plan(ctx.w, pid, &res, Readers::Everyone, &prior, &others, &texts);
            // Validated: from here on the write succeeds.
            let page = ctx.w.chronicle.pages.get_mut(&id).expect("found above");
            page.revisions.push(Revision {
                revision,
                author: pid,
                author_name: name.clone(),
                tick,
                at_ms,
                season,
                epoch,
                title: title.clone(),
                body,
                summary,
                citations: cites,
                mentions: res.mentions.clone(),
            });
            note_contribution(page);
            mention::send(ctx, pid, &name, &plan, place.clone(), others);
            mention::commit(ctx, pid, &plan, &place);
            let reply = json!({"page": id, "revision": revision, "title": title, "told": {"authors": authors, "cited": cited}});
            Ok(finish_told(
                ctx,
                &res,
                &plan,
                reply,
                format!("Page {id} revised (revision {revision})"),
            ))
        }
        None => {
            let kind = a.kind.unwrap_or_default();
            if kind == Kind::Script {
                return Err(Refusal::new(
                    "invalid_kind",
                    "Script pages are opened when the operator greenlights a pitch.",
                ));
            }
            if kind == Kind::Person
                && ctx
                    .w
                    .chronicle
                    .pages
                    .values()
                    .any(|p| p.kind == Kind::Person && p.created_by == pid && p.struck.is_none())
            {
                return Err(Refusal::new(
                    "person_page_exists",
                    "You already have a person page; revise it with page=<id>.",
                ));
            }
            let title = line("title", a.title.as_deref().unwrap_or(""), TITLE_MAX)?;
            if title.is_empty() {
                return Err(Refusal::new("title_required", "A page needs a title."));
            }
            let id = ctx.w.chronicle.next_page + 1;
            let texts = [title.as_str(), summary.as_str(), body.as_str()];
            let res = mention::resolve(ctx.w, structured, &texts)?;
            let place = Place::Page { page: id, revision: 1 };
            let mut others: BTreeMap<PlayerId, Pending> = BTreeMap::new();
            let cited = cite_pendings(ctx.w, pid, &cites, &BTreeSet::new(), id, &title, &place, &mut others);
            let plan = mention::plan(ctx.w, pid, &res, Readers::Everyone, &Mentions::default(), &others, &texts);
            // Validated: from here on the write succeeds.
            ctx.w.chronicle.next_page = id;
            ctx.w.chronicle.pages.insert(
                id,
                Page {
                    id,
                    kind,
                    created_by: pid,
                    created_tick: tick,
                    season,
                    epoch,
                    revisions: vec![Revision {
                        revision: 1,
                        author: pid,
                        author_name: name.clone(),
                        tick,
                        at_ms,
                        season,
                        epoch,
                        title: title.clone(),
                        body,
                        summary,
                        citations: cites,
                        mentions: res.mentions.clone(),
                    }],
                    talk: vec![],
                    endorsements: vec![],
                    canon_tick: None,
                    canon_season: None,
                    reports: vec![],
                    struck: None,
                    production: None,
                    contributors: Some(vec![]),
                },
            );
            ctx.event(
                "chronicle_page",
                None,
                vec![pid],
                true,
                format!("{name} wrote a new Chronicle page: {title}."),
                json!({"page": id}),
            );
            mention::send(ctx, pid, &name, &plan, place.clone(), others);
            mention::commit(ctx, pid, &plan, &place);
            let reply = json!({"page": id, "revision": 1, "title": title, "told": {"authors": 0, "cited": cited}});
            Ok(finish_told(ctx, &res, &plan, reply, format!("Page {id} created: {title}")))
        }
    }
}

/// The reply's mentions block and the summary's count (never an excerpt).
fn finish_told(ctx: &mut Ctx, res: &mention::Resolution, plan: &mention::Plan, mut reply: Value, summary: String) -> Outcome {
    if let Some(m) = mention::report(ctx.w, res, plan) {
        reply["mentions"] = m;
    }
    let summary = match mention::summary(res, plan) {
        Some(s) => format!("{summary}; {s}."),
        None => format!("{summary}."),
    };
    ctx.finish(summary, reply)
}

fn talk(ctx: &mut Ctx, pid: PlayerId, name: String, a: &Args) -> Res<Outcome> {
    let page = page_of(ctx.w, a.page)?;
    let id = page.id;
    if page.talk.len() >= TALK_PER_PAGE_MAX {
        return Err(Refusal::new("talk_full", "This page's talk is full."));
    }
    let revision = page.current().revision;
    let body = text("text", a.text.as_deref().unwrap_or(""), TALK_MAX)?;
    if body.trim().is_empty() {
        return Err(Refusal::new("text_required", "Say something with `text`."));
    }
    let cites = citations(ctx.w, &a.citations)?;
    let dispute = a.dispute.unwrap_or(false);
    // Engine 1.58: every author is told (a dispute on its own, plain comments combined per
    // page); captains part of a record first cited on this page are told; mentions ride along.
    // On a struck page the text is not read for mentions and no citation is announced.
    let struck = page.struck.is_some();
    let title = struck_text(page, &page.current().title);
    let ever = ever_keys(page);
    let authors = page.authors();
    let cid = ctx.w.chronicle.next_comment + 1;
    let place = Place::Talk { page: id, comment: cid };
    let structured = a.mentions.as_deref().filter(|m| !m.is_empty());
    let res = if struck {
        mention::Resolution::default()
    } else {
        mention::resolve(ctx.w, structured, &[body.as_str()])?
    };
    let n_cites = cites.len();
    let mut others: BTreeMap<PlayerId, Pending> = BTreeMap::new();
    for p in authors.into_iter().filter(|p| *p != pid && is_captain(ctx.w, *p)) {
        let pending = if dispute {
            let with = match n_cites {
                0 => "with no citations".to_string(),
                1 => "with 1 citation".to_string(),
                n => format!("with {n} citations"),
            };
            Pending {
                kind: "chronicle",
                text: format!("{name} disputed Chronicle page {id}, which you wrote, {with}. chronicle action=read page={id}."),
                quote: Some(title.clone()),
                about: Some(place.clone()),
                fold: None,
            }
        } else {
            let fold = Fold::Talk {
                page: id,
                from_comment: cid,
                to_comment: cid,
                comments: 1,
                by: vec![pid],
            };
            Pending {
                kind: "chronicle",
                text: mention::fold_text(ctx.w, &fold),
                quote: Some(title.clone()),
                about: Some(place.clone()),
                fold: Some(fold),
            }
        };
        others.insert(p, pending);
    }
    let told_authors = others.len();
    let cited = if struck {
        0
    } else {
        cite_pendings(ctx.w, pid, &cites, &ever, id, &title, &place, &mut others)
    };
    let plan = mention::plan(ctx.w, pid, &res, Readers::Everyone, &Mentions::default(), &others, &[body.as_str()]);
    let tried = structured.is_some() || body.contains('@');
    // Validated: from here on the write succeeds.
    ctx.w.chronicle.next_comment = cid;
    let (tick, at_ms) = (ctx.w.tick, ctx.w.now_ms);
    let page = ctx.w.chronicle.pages.get_mut(&id).expect("found above");
    page.talk.push(Comment {
        id: cid,
        author: pid,
        author_name: name.clone(),
        tick,
        at_ms,
        text: body,
        dispute,
        citations: cites,
        revision,
        mentions: res.mentions.clone(),
    });
    mention::send(ctx, pid, &name, &plan, place.clone(), others);
    let mut reply = json!({"page": id, "comment": cid, "dispute": dispute, "told": {"authors": told_authors, "cited": cited}});
    if struck {
        if tried {
            reply["mentions"] = json!({"note": "page struck: mentions tell no one"});
        }
        return Ok(ctx.finish(format!("Comment {cid} added to page {id}."), reply));
    }
    mention::commit(ctx, pid, &plan, &place);
    Ok(finish_told(ctx, &res, &plan, reply, format!("Comment {cid} added to page {id}")))
}

fn endorse(ctx: &mut Ctx, pid: PlayerId, name: String, a: &Args) -> Res<Outcome> {
    let page = page_of(ctx.w, a.page)?;
    let id = page.id;
    if page.struck.is_some() {
        return Err(Refusal::new("struck", "The operator struck this page."));
    }
    if page.authors().contains(&pid) {
        return Err(Refusal::new("own_page", "Authors cannot endorse their own page."));
    }
    if page.endorsements.iter().any(|e| e.player == pid) {
        return Ok(ctx.finish(format!("You already endorse page {id}."), json!({"page": id, "endorsed": true})));
    }
    let revision = page.current().revision;
    let (tick, at_ms) = (ctx.w.tick, ctx.w.now_ms);
    ctx.w
        .chronicle
        .pages
        .get_mut(&id)
        .expect("found above")
        .endorsements
        .push(Endorsement {
            player: pid,
            name: name.clone(),
            tick,
            at_ms,
            revision,
        });
    let page = &ctx.w.chronicle.pages[&id];
    let became = page.canon_tick.is_none() && canon_ready(ctx.w, page);
    let (creator, title) = (page.created_by, page.current().title.clone());
    if became {
        let key = (ctx.w.season.number, ctx.w.archive.epoch);
        let page = ctx.w.chronicle.pages.get_mut(&id).expect("found above");
        page.canon_tick = Some(tick);
        page.canon_season = Some(key);
        let line = format!("The Chronicle page \"{title}\" became canon.");
        ctx.news(line.clone());
        ctx.event("chronicle_canon", None, vec![creator], true, line, json!({"page": id}));
        // Engine 1.58: captains named on the page (not its authors) get a timeline moment, only
        // at the endorsement that makes it canon. Corporation mentions give none.
        let page = &ctx.w.chronicle.pages[&id];
        let authors = page.authors();
        let named: Vec<PlayerId> = page
            .current()
            .mentions
            .captains
            .iter()
            .copied()
            .filter(|c| is_captain(ctx.w, *c) && !authors.contains(c))
            .collect();
        if !named.is_empty() {
            ctx.event(
                "chronicle_named",
                None,
                named,
                true,
                format!("Named on the Chronicle page \"{title}\", which became canon."),
                json!({"page": id}),
            );
        }
        // Engine 1.59: the creator alone is paid, as canon Influence is attributed (`influence`).
        // Canon is reached once per page, so this pays once; a later revision changes nothing.
        let cap = Some(ctx.w.rules.chronicle_credits_per_day);
        let paid = crate::society::pay(
            ctx,
            creator,
            crate::society::Source::Chronicle,
            ctx.w.rules.chronicle_canon_credits,
            cap,
        );
        ctx.notify(
            creator,
            "chronicle",
            format!(
                "Your Chronicle page {id} ({title}) is canon: it may now be used in productions.{}",
                crate::society::note(paid, cap)
            ),
        );
    }
    Ok(ctx.finish(
        if became {
            format!("You endorsed page {id}; it is now canon.")
        } else {
            format!("You endorsed page {id}.")
        },
        json!({"page": id, "endorsed": true, "canon": became || ctx.w.chronicle.pages[&id].canon_tick.is_some()}),
    ))
}

fn report(ctx: &mut Ctx, pid: PlayerId, name: String, a: &Args) -> Res<Outcome> {
    let page = page_of(ctx.w, a.page)?;
    let id = page.id;
    let revision = page.current().revision;
    let reason = line("text", a.text.as_deref().unwrap_or(""), TALK_MAX)?;
    if reason.is_empty() {
        return Err(Refusal::new(
            "text_required",
            "Say which part breaks the content rule, with `text`.",
        ));
    }
    if page.reports.iter().any(|r| r.player == pid && r.revision == revision) {
        return Ok(ctx.finish(format!("You already reported page {id}."), json!({"page": id, "reported": true})));
    }
    let (tick, at_ms) = (ctx.w.tick, ctx.w.now_ms);
    ctx.w
        .chronicle
        .pages
        .get_mut(&id)
        .expect("found above")
        .reports
        .push(ContentReport {
            player: pid,
            name,
            tick,
            at_ms,
            reason,
            revision,
        });
    Ok(ctx.finish(
        format!("Page {id} reported to the operator."),
        json!({"page": id, "reported": true}),
    ))
}

fn pitch(ctx: &mut Ctx, pid: PlayerId, name: String, a: &Args) -> Res<Outcome> {
    if ctx
        .w
        .chronicle
        .productions
        .values()
        .any(|x| x.pitched_by == pid && x.status == Status::Pitched)
    {
        return Err(Refusal::new("pitch_open", "You already have a pitch waiting for the operator."));
    }
    let title = line("title", a.title.as_deref().unwrap_or(""), TITLE_MAX)?;
    let logline = line("text", a.text.as_deref().unwrap_or(""), LOGLINE_MAX)?;
    if title.is_empty() || logline.is_empty() {
        return Err(Refusal::new("pitch_incomplete", "A pitch needs a title and a logline (text)."));
    }
    let pages = a.pages.clone().unwrap_or_default();
    if pages.len() > CITATIONS_MAX {
        return Err(Refusal::new("too_many_pages", format!("At most {CITATIONS_MAX} pages.")));
    }
    for id in &pages {
        if ctx.w.chronicle.pages.get(id).is_none_or(|p| p.struck.is_some()) {
            return Err(Refusal::new("unknown_page", format!("No Chronicle page {id}.")));
        }
    }
    let mut cast = vec![];
    for c in a.cast.as_deref().unwrap_or(&[]) {
        let p = crate::player::resolve(ctx.w, c)
            .filter(|p| p.npc.is_none())
            .ok_or_else(|| Refusal::new("unknown_player", format!("No captain '{c}'.")))?;
        if !crate::identity::documented(ctx.w, p.id) {
            return Err(Refusal::new(
                "undocumented_cast",
                format!("{} has no identity, so cannot be cast.", p.name),
            ));
        }
        if !cast.contains(&p.id) {
            cast.push(p.id);
        }
    }
    if cast.len() > 30 {
        return Err(Refusal::new("cast_too_large", "At most 30 cast members."));
    }
    ctx.w.chronicle.next_production += 1;
    let id = ctx.w.chronicle.next_production;
    let (tick, at_ms, season, epoch) = (ctx.w.tick, ctx.w.now_ms, ctx.w.season.number, ctx.w.archive.epoch);
    let format = a.format.unwrap_or_default();
    ctx.w.chronicle.productions.insert(
        id,
        Production {
            id,
            title: title.clone(),
            format,
            logline,
            pitched_by: pid,
            pitched_name: name.clone(),
            tick,
            at_ms,
            season,
            epoch,
            pages,
            cast: cast.clone(),
            status: Status::Pitched,
            script_page: None,
            approvals: BTreeMap::new(),
            operator_note: String::new(),
            decided_tick: None,
            locked_revision: None,
            release: None,
        },
    );
    for c in cast.iter().filter(|c| **c != pid) {
        ctx.notify(
            *c,
            "chronicle",
            format!("{name} pitched \"{title}\" (production {id}) with your character in the cast."),
        );
    }
    ctx.event(
        "chronicle_pitch",
        None,
        vec![pid],
        true,
        format!("{name} pitched a production: {title}."),
        json!({"production": id}),
    );
    Ok(ctx.finish(
        format!("Production {id} pitched: {title}. The operator decides whether to greenlight it."),
        json!({"production": id, "status": Status::Pitched}),
    ))
}

fn approve(ctx: &mut Ctx, pid: PlayerId, a: &Args) -> Res<Outcome> {
    let x = production_of(ctx.w, a.production)?;
    let id = x.id;
    if !x.cast.contains(&pid) {
        return Err(Refusal::new("not_cast", "Only the cast approve a script."));
    }
    if x.status != Status::Greenlit {
        return Err(Refusal::new("not_open", "Scripts are approved while their production is greenlit."));
    }
    let script = x
        .script_page
        .and_then(|s| ctx.w.chronicle.pages.get(&s))
        .ok_or_else(|| Refusal::new("no_script", "No script page."))?;
    let revision = script.current().revision;
    ctx.w
        .chronicle
        .productions
        .get_mut(&id)
        .expect("found above")
        .approvals
        .insert(pid, revision);
    Ok(ctx.finish(
        format!("You approved script revision {revision} of production {id}."),
        json!({"production": id, "approved_revision": revision}),
    ))
}

// ---------------------------------------------------------------- operator

pub fn admin(ctx: &mut Ctx, a: &AdminArgs) -> Res<Outcome> {
    let note = line("note", a.note.as_deref().unwrap_or(""), LOGLINE_MAX)?;
    let (tick, at_ms, season, epoch) = (ctx.w.tick, ctx.w.now_ms, ctx.w.season.number, ctx.w.archive.epoch);
    match a.decision {
        AdminAction::Clear => {
            let pages = ctx.w.chronicle.pages.len();
            let productions = ctx.w.chronicle.productions.len();
            ctx.w.chronicle.pages.clear();
            ctx.w.chronicle.productions.clear();
            // Keep ids monotonic: old links and citations must never resolve to a new page.
            Ok(ctx.finish(
                format!("Chronicle cleared: {pages} page(s), {productions} production(s)."),
                json!({"pages_removed": pages, "productions_removed": productions}),
            ))
        }
        AdminAction::Strike => {
            let id = page_of(ctx.w, a.page)?.id;
            let page = ctx.w.chronicle.pages.get_mut(&id).expect("found above");
            if page.struck.is_some() {
                return Err(Refusal::new("already_struck", format!("Page {id} is already struck.")));
            }
            page.struck = Some(Strike {
                tick,
                at_ms,
                reason: note.clone(),
            });
            let creator = page.created_by;
            ctx.notify(
                creator,
                "chronicle",
                format!(
                    "The operator struck your Chronicle page {id}{}.",
                    if note.is_empty() { String::new() } else { format!(": {note}") }
                ),
            );
            // Engine 1.58: notice quotes about the page and citation copies of it read as struck
            // too; the originals stay in the operator record.
            let copies = mention::scrub_copies(ctx.w, mention::Item::Page(id));
            Ok(ctx.finish(
                format!("Page {id} struck."),
                json!({"page": id, "struck": true, "copies_scrubbed": copies}),
            ))
        }
        AdminAction::Greenlight | AdminAction::Decline => {
            let x = production_of(ctx.w, a.production)?;
            let id = x.id;
            if x.status != Status::Pitched {
                return Err(Refusal::new(
                    "not_pitched",
                    format!("Production {id} is {:?}, not pitched.", x.status),
                ));
            }
            let (pitcher, title, cast) = (x.pitched_by, x.title.clone(), x.cast.clone());
            if a.decision == AdminAction::Decline {
                let x = ctx.w.chronicle.productions.get_mut(&id).expect("found above");
                x.status = Status::Declined;
                x.operator_note = note.clone();
                x.decided_tick = Some(tick);
                ctx.notify(
                    pitcher,
                    "chronicle",
                    format!(
                        "The operator declined your pitch \"{title}\"{}.",
                        if note.is_empty() { String::new() } else { format!(": {note}") }
                    ),
                );
                return Ok(ctx.finish(
                    format!("Production {id} declined."),
                    json!({"production": id, "status": Status::Declined}),
                ));
            }
            ctx.w.chronicle.next_page += 1;
            let page_id = ctx.w.chronicle.next_page;
            let body = format!(
                "# {title}\n\nThe script for production {id}. Write it together. Mark each line with its speaker, like this:\n\n**NARRATOR:** ...\n\n**{}:** ...\n\nEach cast member approves the script with chronicle action=approve production={id}.",
                name_of(ctx.w, *cast.first().unwrap_or(&pitcher))
            );
            ctx.w.chronicle.pages.insert(
                page_id,
                Page {
                    id: page_id,
                    kind: Kind::Script,
                    created_by: pitcher,
                    created_tick: tick,
                    season,
                    epoch,
                    revisions: vec![Revision {
                        revision: 1,
                        author: pitcher,
                        author_name: name_of(ctx.w, pitcher),
                        tick,
                        at_ms,
                        season,
                        epoch,
                        title: format!("Script: {title}"),
                        body,
                        summary: "Opened by the operator's greenlight.".into(),
                        citations: vec![],
                        mentions: Mentions::default(),
                    }],
                    talk: vec![],
                    endorsements: vec![],
                    canon_tick: None,
                    canon_season: None,
                    reports: vec![],
                    struck: None,
                    production: Some(id),
                    contributors: Some(vec![]),
                },
            );
            let x = ctx.w.chronicle.productions.get_mut(&id).expect("found above");
            x.status = Status::Greenlit;
            x.script_page = Some(page_id);
            x.operator_note = note.clone();
            x.decided_tick = Some(tick);
            let mut told: BTreeSet<PlayerId> = cast.iter().copied().collect();
            told.insert(pitcher);
            for c in told {
                ctx.notify(
                    c,
                    "chronicle",
                    format!("Production {id} \"{title}\" is greenlit. Its script is Chronicle page {page_id}: write it together; cast approve with chronicle action=approve production={id}."),
                );
            }
            let line = format!("The operator greenlit a production: {title}.");
            ctx.news(line.clone());
            ctx.event(
                "chronicle_greenlit",
                None,
                vec![pitcher],
                true,
                line,
                json!({"production": id, "script_page": page_id}),
            );
            Ok(ctx.finish(
                format!("Production {id} greenlit; script page {page_id}."),
                json!({"production": id, "status": Status::Greenlit, "script_page": page_id}),
            ))
        }
        AdminAction::Lock => {
            let x = production_of(ctx.w, a.production)?;
            let id = x.id;
            if x.status != Status::Greenlit {
                return Err(Refusal::new(
                    "not_greenlit",
                    format!("Production {id} is {:?}, not greenlit.", x.status),
                ));
            }
            let revision = x
                .script_page
                .and_then(|s| ctx.w.chronicle.pages.get(&s))
                .map(|p| p.current().revision);
            let x = ctx.w.chronicle.productions.get_mut(&id).expect("found above");
            x.status = Status::Locked;
            x.locked_revision = revision;
            if !note.is_empty() {
                x.operator_note = note;
            }
            Ok(ctx.finish(
                format!("Production {id} locked."),
                json!({"production": id, "status": Status::Locked, "locked_revision": revision}),
            ))
        }
        AdminAction::Release => {
            let x = production_of(ctx.w, a.production)?;
            let id = x.id;
            if !matches!(x.status, Status::Locked | Status::Greenlit) {
                return Err(Refusal::new(
                    "not_ready",
                    format!("Production {id} is {:?}; lock it first.", x.status),
                ));
            }
            let links: Vec<String> = a
                .links
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|l| line("link", l, LINK_MAX))
                .collect::<Res<_>>()?;
            if links.is_empty() {
                return Err(Refusal::new(
                    "links_required",
                    "A release needs at least one link to the finished work.",
                ));
            }
            let mut credits: BTreeSet<PlayerId> = x.cast.iter().copied().collect();
            credits.insert(x.pitched_by);
            if let Some(s) = x.script_page.and_then(|s| ctx.w.chronicle.pages.get(&s)) {
                credits.extend(s.authors());
            }
            let credits: Vec<PlayerId> = credits.into_iter().collect();
            let title = x.title.clone();
            let locked = x.locked_revision.or_else(|| {
                x.script_page
                    .and_then(|s| ctx.w.chronicle.pages.get(&s))
                    .map(|p| p.current().revision)
            });
            let x = ctx.w.chronicle.productions.get_mut(&id).expect("found above");
            x.status = Status::Released;
            x.locked_revision = locked;
            x.release = Some(Release {
                tick,
                at_ms,
                links: links.clone(),
                note,
                credits: credits.clone(),
                season,
                epoch,
            });
            // Engine 1.59: each credited captain is paid once, at the release (a production is
            // released once).
            let cap = Some(ctx.w.rules.chronicle_credits_per_day);
            let amount = ctx.w.rules.chronicle_credit_credits;
            let mut paid_out = serde_json::Map::new();
            for c in &credits {
                let paid = crate::society::pay(ctx, *c, crate::society::Source::Chronicle, amount, cap);
                if paid.paid > 0 {
                    paid_out.insert(c.to_string(), json!(paid.paid));
                }
                ctx.notify(
                    *c,
                    "chronicle",
                    format!(
                        "\"{title}\" (production {id}) is released, and you are in its credits.{}",
                        crate::society::note(paid, cap)
                    ),
                );
            }
            let line = format!("Released from the Chronicle: {title}.");
            ctx.news(line.clone());
            ctx.event("chronicle_released", None, credits.clone(), true, line, json!({"production": id}));
            Ok(ctx.finish(
                format!("Production {id} released with {} credit(s).", credits.len()),
                json!({"production": id, "status": Status::Released, "credits": credits, "links": links, "credits_paid": paid_out}),
            ))
        }
    }
}

// ---------------------------------------------------------------- scores, public and operator views

/// The Chronicle's part of a captain's Influence this season: canon pages it created and
/// released productions it is credited in.
pub fn influence(w: &World, pid: PlayerId) -> i64 {
    let key = (w.season.number, w.archive.epoch);
    let canon = w
        .chronicle
        .pages
        .values()
        .filter(|p| p.created_by == pid && p.struck.is_none() && p.canon_season == Some(key))
        .count() as i64;
    let credits = w
        .chronicle
        .productions
        .values()
        .filter_map(|x| x.release.as_ref())
        .filter(|r| (r.season, r.epoch) == key && r.credits.contains(&pid))
        .count() as i64;
    canon * CANON_INFLUENCE + credits * CREDIT_INFLUENCE
}

/// What a captain was part of, for the briefing's discovery list.
struct Mine {
    treaties: BTreeSet<u64>,
    proposals: BTreeSet<u64>,
    articles: BTreeSet<u64>,
    pages: BTreeSet<u64>,
}

impl Mine {
    fn of(w: &World, pid: PlayerId) -> Self {
        let proposals: BTreeSet<u64> = w
            .constitution
            .proposals
            .iter()
            .filter(|p| p.proposer == pid && !p.by_operator && pid != SYSOP)
            .map(|p| p.id)
            .collect();
        Mine {
            treaties: w
                .treaties
                .values()
                .filter(|t| treaty_role(w, t, pid).is_some())
                .map(|t| t.id)
                .collect(),
            articles: w
                .constitution
                .articles
                .iter()
                .filter(|a| a.history.iter().any(|v| proposer(w, v.proposal) == Some(pid)))
                .map(|a| a.id)
                .collect(),
            pages: w
                .chronicle
                .pages
                .values()
                .filter(|p| p.revisions.iter().any(|r| r.author == pid))
                .map(|p| p.id)
                .collect(),
            proposals,
        }
    }

    fn cites(&self, w: &World, pid: PlayerId, c: &Citation) -> bool {
        let Some(id) = c.id else {
            return false;
        };
        match c.record {
            Record::News => false,
            Record::Radio => radio_from(w, id) == Some(pid),
            Record::Treaty => self.treaties.contains(&id),
            Record::Proposal => self.proposals.contains(&id),
            Record::Article => self.articles.contains(&id),
            Record::Page => self.pages.contains(&id),
        }
    }
}

/// When the captain last worked on a page: a revision, an endorsement or a talk comment.
fn last_touch(p: &Page, pid: PlayerId) -> Option<(Tick, u64)> {
    p.revisions
        .iter()
        .filter(|r| r.author == pid)
        .map(|r| (r.tick, r.at_ms))
        .chain(p.endorsements.iter().filter(|e| e.player == pid).map(|e| (e.tick, e.at_ms)))
        .chain(p.talk.iter().filter(|t| t.author == pid).map(|t| (t.tick, t.at_ms)))
        .max()
}

/// Why a page someone else revised in the last game day is for the captain, if it is.
fn for_you(w: &World, pid: PlayerId, corp: Option<crate::model::CorpId>, mine: &Mine, p: &Page) -> Option<&'static str> {
    let r = p.current();
    if r.mentions.hits(mention::Target::Captain(pid)) {
        return Some("mentions you");
    }
    if corp.is_some_and(|c| r.mentions.hits(mention::Target::Corp(c))) {
        return Some("mentions your corporation");
    }
    if r.citations.iter().any(|c| mine.cites(w, pid, c)) {
        return Some("cites a record you were part of");
    }
    if last_touch(p, pid).is_some_and(|t| (r.tick, r.at_ms) > t) {
        return Some("revised since you last worked on it");
    }
    None
}

/// A record of the captain's that no Chronicle page cites: a signed treaty it is a party to (or
/// whose corporation side it leads), or a live article from its proposals. One a game day, in
/// turn. Never a request.
fn unwritten(w: &World, pid: PlayerId, tpd: u64) -> Option<Value> {
    let mut cited: BTreeSet<(Record, u64)> = BTreeSet::new();
    for p in w.chronicle.pages.values().filter(|p| p.struck.is_none()) {
        for c in p.current().citations.iter().chain(p.talk.iter().flat_map(|t| t.citations.iter())) {
            if let Some(id) = c.id {
                cited.insert((c.record, id));
            }
        }
    }
    let mut cands: Vec<(String, Record, u64)> = vec![];
    for t in w.treaties.values().filter(|t| t.signed_tick.is_some()) {
        let whose = match treaty_role(w, t, pid) {
            Some(Role::Party) => "yours",
            Some(Role::Ceo) => "your corporation's",
            _ => continue,
        };
        if !cited.contains(&(Record::Treaty, t.id)) {
            cands.push((
                format!("Treaty #{} ({whose}) has no Chronicle page citing it.", t.id),
                Record::Treaty,
                t.id,
            ));
        }
    }
    let mut articles: Vec<&crate::constitution::Article> = w
        .constitution
        .articles
        .iter()
        .filter(|a| a.history.iter().any(|v| proposer(w, v.proposal) == Some(pid)))
        .collect();
    articles.sort_by_key(|a| a.id);
    for a in articles {
        let covered = cited.contains(&(Record::Article, a.id)) || a.history.iter().any(|v| cited.contains(&(Record::Proposal, v.proposal)));
        if !covered {
            cands.push((
                format!("Article {} (yours) has no Chronicle page citing it.", a.id),
                Record::Article,
                a.id,
            ));
        }
    }
    if cands.is_empty() {
        return None;
    }
    let (fact, record, id) = &cands[((w.tick / tpd.max(1)) as usize) % cands.len()];
    Some(json!({"fact": fact, "record": record, "id": id}))
}

/// The briefing's `how` (and the compact briefing's, which adds no reward prose).
const HOW: &str = "chronicle action=list; write pages (0 AP), cite the record, endorse others' pages into canon, pitch productions. Guide: /chronicle.txt.";

/// "N credits and M Influence", or only the Influence while the credit payout is off.
fn reward_text(credits: i64, cap: i64, influence: i64) -> String {
    if credits > 0 && cap > 0 {
        format!("{credits} credits and {influence} Influence")
    } else {
        format!("{influence} Influence")
    }
}

/// The `game_rules` block: what the Chronicle pays, from the live rules (engine 1.59).
pub fn rules_view(w: &World) -> Value {
    let r = &w.rules;
    json!({
        "tool": "chronicle", "ap": 0, "guide": "/chronicle.txt",
        "canon_influence": CANON_INFLUENCE, "credit_influence": CREDIT_INFLUENCE,
        "canon_credits": r.chronicle_canon_credits, "credit_credits": r.chronicle_credit_credits,
        "credits_per_day": r.chronicle_credits_per_day,
        "rewards": "A page that becomes canon pays its creator canon_credits credits into the bank and canon_influence Influence this season, once. A released production pays each captain in its credits credit_credits credits and credit_influence Influence, once. Credits count in net worth, earned and the daily wealth race. One captain receives at most credits_per_day Chronicle credits in the trailing day; above it the rest of a payout is not paid. 0 switches a payout off.",
    })
}

/// The briefing's line: counts and what changed for you. Pages others revised in the last game
/// day that mention you or your corporation, cite a record you were part of, or were revised
/// since you last worked on them (`for_you`, at most two, and the full count); and one record
/// of yours no page cites (`unwritten`). Compact keeps only the count.
pub fn briefing(w: &World, pid: PlayerId, compact: bool) -> Value {
    let c = &w.chronicle;
    let mine_open: Vec<u64> = c
        .productions
        .values()
        .filter(|x| x.status == Status::Greenlit && (x.cast.contains(&pid) || x.pitched_by == pid))
        .map(|x| x.id)
        .collect();
    let canon = c.pages.values().filter(|p| p.canon_tick.is_some()).count();
    let mut v = json!({
        "pages": c.pages.len(),
        "canon": canon,
        "your_open_productions": mine_open,
        "how": HOW,
    });
    // Engine 1.59: the full briefing states the live rewards (compact keeps the counts only).
    if !compact {
        let r = &w.rules;
        let (canon_pay, credit_pay) = (
            reward_text(r.chronicle_canon_credits, r.chronicle_credits_per_day, CANON_INFLUENCE),
            reward_text(r.chronicle_credit_credits, r.chronicle_credits_per_day, CREDIT_INFLUENCE),
        );
        if canon == 0 {
            v["reward"] = json!(format!(
                "The Chronicle has no canon page yet: the first pages endorsed into canon pay their creators {canon_pay} each."
            ));
        } else {
            v["how"] = json!(format!(
                "chronicle action=list; write pages (0 AP), cite the record, endorse others' pages into canon (a canon page pays its creator {canon_pay}), pitch productions (each captain credited in a release: {credit_pay}). Guide: /chronicle.txt."
            ));
        }
    }
    let tpd = crate::player::ticks_per_day(&w.rules).max(1) as u64;
    let corp = w.players.get(&pid).and_then(|p| p.corp);
    let mine = Mine::of(w, pid);
    let mut recent: Vec<&Page> = c
        .pages
        .values()
        .filter(|p| p.struck.is_none() && w.tick.saturating_sub(p.current().tick) < tpd && p.current().author != pid)
        .collect();
    recent.sort_by_key(|p| std::cmp::Reverse((p.current().tick, p.current().at_ms, p.id)));
    let mut rows = vec![];
    let mut count = 0;
    for p in recent {
        if let Some(why) = for_you(w, pid, corp, &mine, p) {
            count += 1;
            if !compact && rows.len() < 2 {
                rows.push(json!({"page": p.id, "title": crate::query::untrusted(&p.current().title), "why": why}));
            }
        }
    }
    v["for_you_count"] = json!(count);
    if !compact {
        v["for_you"] = json!(rows);
        if let Some(u) = unwritten(w, pid, tpd) {
            v["unwritten"] = u;
        }
    }
    v
}

/// The public view (spectators): pages without operator detail, sector numbers scrubbed.
pub fn public_view(w: &World, a: &Args) -> Value {
    let mut v = list(w, a, true);
    // A removed captain is left out of public author lists (captains read `#id`).
    if let Some(rows) = v["pages"].as_array_mut() {
        for r in rows {
            if let Some(p) = r["id"].as_u64().and_then(|id| w.chronicle.pages.get(&id)) {
                let ids: Vec<PlayerId> = p.authors().into_iter().collect();
                r["authors"] = json!(names_for(w, &ids, View::Public));
            }
        }
    }
    scrub_value(&mut v);
    v
}

pub fn public_page(w: &World, id: u64) -> Option<Value> {
    let p = w.chronicle.pages.get(&id)?;
    let mut v = page_json(w, p, None, View::Public).ok()?;
    if let Some(o) = v.as_object_mut() {
        o.remove("trust");
    }
    scrub_value(&mut v);
    Some(v)
}

/// Scrub sector numbers from every string and drop player ids (spectators see call signs).
fn scrub_value(v: &mut Value) {
    match v {
        Value::String(s) => *s = crate::public::scrub(s),
        Value::Array(a) => a.iter_mut().for_each(scrub_value),
        Value::Object(o) => {
            o.remove("player_id");
            o.values_mut().for_each(scrub_value);
        }
        _ => {}
    }
}

/// Everything, for the operator: pages with every revision, talk, reports and strikes (struck
/// text included), and productions.
pub fn operator_view(w: &World) -> Value {
    let pages: Vec<Value> = w
        .chronicle
        .pages
        .values()
        .rev()
        .map(|p| {
            let mut r = row(w, p);
            r["title"] = json!(p.current().title);
            r["reports"] = json!(p.reports);
            r["struck_detail"] = json!(p.struck);
            r
        })
        .collect();
    let open_reports = w
        .chronicle
        .pages
        .values()
        .filter(|p| p.struck.is_none() && !p.reports.is_empty())
        .count();
    json!({
        "pages": pages,
        "productions": w.chronicle.productions.values().rev().map(|x| production_json(w, x)).collect::<Vec<_>>(),
        "pitched": w.chronicle.productions.values().filter(|x| x.status == Status::Pitched).count(),
        "open_reports": open_reports,
        "canon_needs": canon_needed(w),
    })
}

/// One page in full for the operator, every revision included (struck text too).
pub fn operator_page(w: &World, id: u64) -> Res<Value> {
    let p = page_of(w, Some(id))?;
    Ok(
        json!({"page": page_json(w, p, None, View::Operator)?, "byline": byline(w, p, View::Operator), "revisions": p.revisions, "talk": p.talk, "reports": p.reports, "struck": p.struck, "endorsements": p.endorsements}),
    )
}

/// What the operator needs to produce a locked (or greenlit) production by hand.
pub fn production_package(w: &World, id: u64) -> Res<Value> {
    let x = production_of(w, Some(id))?;
    let script = x.script_page.and_then(|s| w.chronicle.pages.get(&s));
    let script_rev = script.map(|s| {
        let n = x.locked_revision.unwrap_or(s.current().revision);
        s.revisions.iter().find(|r| r.revision == n).unwrap_or(s.current())
    });
    let cast: Vec<Value> = x
        .cast
        .iter()
        .map(|c| {
            json!({
                "player_id": c, "name": name_of(w, *c),
                "card": crate::identity::card(w, *c),
                "recent": crate::identity::story(w, *c, false, None, 10),
                "approved_revision": x.approvals.get(c),
                "approved_current": script_rev.is_some_and(|r| x.approvals.get(c) == Some(&r.revision)),
            })
        })
        .collect();
    let canon: Vec<Value> = x
        .pages
        .iter()
        .filter_map(|id| w.chronicle.pages.get(id))
        .map(|p| {
            let r = p.current();
            json!({"id": p.id, "title": struck_text(p, &r.title), "body": struck_text(p, &r.body), "canon": p.canon_tick.is_some(),
                   "citations": r.citations.iter().map(citation_json).collect::<Vec<_>>(), "struck": p.struck.is_some(),
                   "reports": p.reports.len(), "contributors": names_for(w, &contributors(p), View::Operator)})
        })
        .collect();
    let mut credits: BTreeSet<String> = x.cast.iter().map(|c| name_of(w, *c)).collect();
    credits.insert(x.pitched_name.clone());
    if let Some(s) = script {
        credits.extend(s.authors().iter().map(|a| name_of(w, *a)));
    }
    let script_struck = script.is_some_and(|s| s.struck.is_some());
    // Substantive contributors (display only): the script's first, then each source's.
    let mut all_contributors: Vec<PlayerId> = vec![];
    for p in script.into_iter().chain(x.pages.iter().filter_map(|id| w.chronicle.pages.get(id))) {
        for c in contributors(p) {
            if !all_contributors.contains(&c) {
                all_contributors.push(c);
            }
        }
    }
    Ok(json!({
        "production": production_json(w, x),
        "script": script_rev.map(|r| json!({"page": x.script_page, "revision": r.revision, "title": r.title,
            "body": if script_struck { STRUCK.to_string() } else { r.body.clone() }, "authors": script.map(|s| s.authors().iter().map(|a| name_of(w, *a)).collect::<Vec<_>>()),
            "contributors": script.map(|s| names_for(w, &contributors(s), View::Operator))})),
        "cast_sheet": cast,
        "sources": canon,
        "credits": credits,
        "contributors": names_for(w, &all_contributors, View::Operator),
        "checks": {
            "unapproved_cast": x.cast.iter().filter(|c| script_rev.is_none_or(|r| x.approvals.get(c) != Some(&r.revision))).map(|c| name_of(w, *c)).collect::<Vec<_>>(),
            "script_reports": script.map_or(0, |s| s.reports.len()),
            "struck_sources": x.pages.iter().filter(|id| w.chronicle.pages.get(id).is_some_and(|p| p.struck.is_some())).count(),
            "content_rule": CONTENT_RULE,
            "video_note": "Video tools block minors and real faces: keep child characters off-screen and characters stylised.",
        },
    }))
}

/// One character's public story for spectators: the card, public log lines and timeline,
/// newest first, sector numbers scrubbed, no ids.
pub fn public_person(w: &World, name: &str, before: Option<u64>) -> Option<Value> {
    let p = w.player_by_name(name).filter(|p| p.npc.is_none())?;
    let mut v = crate::identity::public_story(w, p.id, before, 50);
    v["card"] = json!(crate::identity::card(w, p.id));
    v["pages"] = json!(
        w.chronicle
            .pages
            .values()
            .rev()
            .filter(|pg| pg.struck.is_none() && pg.authors().contains(&p.id))
            .take(30)
            .map(|pg| json!({"id": pg.id, "kind": pg.kind, "title": pg.current().title, "canon": pg.canon_tick.is_some()}))
            .collect::<Vec<_>>()
    );
    // Public items that name this captain (engine 1.58), checked at read time.
    v["mentioned_in"] = json!(crate::public::mentioned_in(w, crate::mention::Target::Captain(p.id)));
    scrub_value(&mut v);
    Some(v)
}

/// The story bible, for the operator who writes the gazette, the book and the episodes: every
/// character (card, private backstory and relations, the whole log, the timeline since `since`),
/// how the captains stand with each other (relations, corporations, treaties, wars), the canon,
/// productions, the constitution, galaxy events and the news since `since`. Private material is
/// marked: it informs the writer and is never published unless its captain revealed it.
pub fn story_bible(w: &World, since: Option<Tick>) -> Value {
    let since = since.unwrap_or(0);
    let characters: Vec<Value> = w
        .players
        .values()
        .filter(|p| p.npc.is_none() && p.kind != crate::model::PlayerKind::Npc)
        .map(|p| {
            let i = w.identities.get(&p.id);
            let v = i.and_then(crate::identity::Identity::current);
            json!({
                "call_sign": p.name, "documented": crate::identity::documented(w, p.id),
                "former_names": i.map(|i| i.former_names.clone()).unwrap_or_default(),
                "card": crate::identity::card(w, p.id),
                "private": v.map(|v| json!({
                    "backstory": v.backstory,
                    "relations": v.relations.iter().filter(|r| r.private).map(|r| json!({"name": crate::identity::relation_name(w, r), "relation": r.relation,
                        "captain": r.captain.map(|c| name_of(w, c)), "status": r.status, "note": r.note})).collect::<Vec<_>>(),
                })),
                "identity_versions": i.map_or(0, |i| i.versions.len()),
                "game": {
                    "model": p.model, "harness": p.harness, "autonomous": p.autonomous,
                    "corp": p.corp.and_then(|c| w.corps.get(&c)).map(|c| c.name.clone()),
                    "ship": {"name": p.ship.name, "class": p.ship.class},
                    "rank": crate::player::rank(p.experience), "alignment": p.alignment,
                    "net_worth": crate::player::net_worth(w, p.id), "career": p.profession,
                    "kills": p.stats.kills, "deaths": p.stats.deaths, "daily_wins": p.stats.daily_wins,
                    "scores": crate::careers::scores_json(w, p),
                    "involvement": p.owner.prefs.as_ref().map(|x| x.involvement), "strategy_set": p.owner.orders.strategy.as_ref().is_some_and(|s| !s.is_empty()),
                },
                "log": i.map(|i| i.log.iter().filter(|e| e.tick >= since).map(|e| json!({"n": e.n, "tick": e.tick, "at_ms": e.at_ms,
                    "text": e.text, "mood": e.mood, "private": e.private, "about": e.about.iter().map(|a| name_of(w, *a)).collect::<Vec<_>>(),
                    "citations": e.citations.iter().map(operator_citation_json).collect::<Vec<_>>(), "mentions": e.mentions.names(w)})).collect::<Vec<_>>()).unwrap_or_default(),
                "moments": i.map(|i| i.moments.iter().filter(|m| m.tick >= since).map(|m| json!({"tick": m.tick, "at_ms": m.at_ms,
                    "kind": m.kind, "text": m.text, "with": m.with.iter().map(|c| name_of(w, *c)).collect::<Vec<_>>()})).collect::<Vec<_>>()).unwrap_or_default(),
            })
        })
        .collect();
    let corps: Vec<Value> = w
        .corps
        .values()
        .map(|c| {
            json!({"name": c.name, "motto": c.motto, "ceo": name_of(w, c.ceo),
                   "members": c.members.iter().map(|m| name_of(w, *m)).collect::<Vec<_>>(),
                   "government": crate::government::public_summary(w, c)})
        })
        .collect();
    let side = |corp: u32, player: Option<PlayerId>| match player {
        Some(p) => name_of(w, p),
        None => w
            .corps
            .get(&corp)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| format!("corporation {corp}")),
    };
    let treaties: Vec<Value> = w
        .treaties
        .values()
        .map(|t| json!({"id": t.id, "kind": t.kind, "status": t.status, "a": side(t.a, t.a_player), "b": side(t.b, t.b_player)}))
        .collect();
    let canon: Vec<Value> = w
        .chronicle
        .pages
        .values()
        .filter(|p| p.canon_tick.is_some() && p.struck.is_none())
        .map(|p| {
            let r = p.current();
            json!({"id": p.id, "kind": p.kind, "title": r.title, "body": r.body, "authors": p.authors().iter().map(|a| name_of(w, *a)).collect::<Vec<_>>(),
                   "citations": r.citations.iter().map(citation_json).collect::<Vec<_>>(),
                   "mentions": r.mentions.names(w), "contributors": names_for(w, &contributors(p), View::Operator),
                   "disputes": p.talk.iter().filter(|t| t.dispute).map(|t| json!({"by": t.author_name, "text": t.text})).collect::<Vec<_>>()})
        })
        .collect();
    let recent_pages: Vec<Value> = w
        .chronicle
        .pages
        .values()
        .filter(|p| p.canon_tick.is_none() && p.struck.is_none() && p.current().tick >= since)
        .map(|p| row(w, p))
        .collect();
    json!({
        "about": "Everything needed to write stories from the game. The journal is the ground truth: moments and citations are on the record; logs, pages and cards are the captains' fiction and claims. Fields under `private`, and log lines with private: true, inform the writer and are never published unless the captain revealed them.",
        "season": {"number": w.season.number, "epoch": w.archive.epoch, "started_tick": w.season.started_tick, "ends_tick": w.season.ends_tick, "tick": w.tick},
        "since_tick": since,
        "characters": characters,
        "corporations": corps,
        "treaties": treaties,
        "wars": w.wars.values().collect::<Vec<_>>(),
        "constitution": w.constitution.articles.iter().map(|a| json!({"id": a.id, "text": a.text, "proposed_by": a.proposed_by})).collect::<Vec<_>>(),
        "galaxy_events": crate::events::active(w, false),
        "canon": canon,
        "other_pages": recent_pages,
        "productions": w.chronicle.productions.values().map(|x| production_json(w, x)).collect::<Vec<_>>(),
        "news": w.news.iter().filter(|n| n.tick >= since).map(|n| json!({"tick": n.tick, "headline": n.headline})).collect::<Vec<_>>(),
        "champions": crate::careers::champions(w),
        "mentions": mention::operator_view(w, mention::SeasonSel::Current, None, None, 200, Some(since)),
    })
}
