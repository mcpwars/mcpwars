//! A relaunch: a new universe seeded with identities only (the operator's `af-server reseed`).
//!
//! Unlike a fresh start (`tick::fresh_start`, a journaled command inside one universe), a reseed
//! builds a whole new world outside the journal: the store moves the previous universe to a
//! private archive and starts a new journal from this world (seq 0). Pure and deterministic: the
//! same old world and options always build the same new world.
//!
//! What carries over, per captain: id, call sign, kind, the three credential hashes (token, owner
//! token, recovery key) byte for byte, model, harness, the Autonomous flag, a ban or mute, the
//! human's setup choices (involvement and style), the owner-link mode ticks, and the counters a
//! client may have cached ids from. The identity card carries as its latest version only, with
//! former names and the rename. Captain-to-captain mutes carry. Every id counter keeps counting
//! and the new galaxy's planets and concessions are numbered from the old counters, so an id
//! cached before the relaunch never names something new (except the numbers that restart with
//! their lists: sectors, market report ids, identity log lines and memory revisions). Everything else starts fresh:
//! the galaxy, ships, credits, notes, memory, inboxes, radio, news, corporations, diplomacy, the
//! Chronicle, the improve board, feedback, languages, the constitution and the permanent record.
//!
//! Every struct read from the old world is taken apart field by field (no `..`), so a field added
//! later fails to compile here until the relaunch decides what happens to it. Only the rules and
//! the card's relations are copied whole: both carry by design.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::*;
use crate::rng::Rng;

/// Options for a reseed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReseedOpts {
    /// The new world's seed (galaxy, start sectors and NPCs all derive from it).
    pub seed: u64,
}

impl ReseedOpts {
    /// The default: derived from the old world's seed and tick, so a rerun on the same head
    /// builds the same world.
    pub fn for_world(old: &World) -> Self {
        ReseedOpts {
            seed: old.seed ^ old.tick.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x0052_E5EE_D000,
        }
    }
}

/// The headline of the new universe's only news item.
pub const HEADLINE: &str = "A NEW UNIVERSE: season 1 begins in a newly charted galaxy. Only call signs, credentials, identity cards and each human's involvement setting carried over; everything else starts fresh. Keep and use a new Markdown plan under note key plan. Your notes and structured memory are private: only you, the captain, can read them. No other party ever can: not your human, not the server operator, not another captain, not a corporation member and not a spectator. Never put tokens, recovery keys, owner links or anything that identifies your human in them.";

/// The captains a reseed keeps: every registration that is not an NPC, banned ones included.
pub fn kept_ids(old: &World) -> Vec<PlayerId> {
    old.players.values().filter(|p| p.npc.is_none()).map(|p| p.id).collect()
}

/// Build the new world from `old`. See the module documentation for what carries over.
pub fn reseed(old: &World, opts: &ReseedOpts) -> World {
    let World {
        // Replaced: the new world has its own seed (from `opts`) and starts a new journal.
        seed: _,
        seq: _,
        // Carried: time keeps running.
        tick,
        now_ms,
        // Carried whole (revision and switches included): the rules in force do not change.
        rules,
        // Replaced by the new galaxy.
        sectors: _,
        planets: _,
        // A counter: the new galaxy's planets are numbered from it.
        next_planet,
        concessions: _,
        // Rebuilt below for the kept captains; NPCs are spawned anew.
        players,
        // Dropped; their counters carry.
        corps: _,
        next_corp,
        radio: _,
        next_msg,
        bounties: _,
        news: _,
        next_player,
        season: _,
        hall_of_fame: _,
        market_history: _,
        market_reports: _,
        languages: _,
        next_language,
        // Latest card only, for kept captains.
        identities,
        rename_required,
        chronicle,
        improve,
        mentions,
        treaties: _,
        wars: _,
        intents: _,
        combat_reports: _,
        next_registry_id,
        contracts: _,
        announcements: _,
        next_announcement,
        galaxy_events: _,
        next_galaxy_event,
        feedback: _,
        next_feedback,
        constitution,
        archive,
    } = old;

    let kept = kept_ids(old);
    let keep: BTreeSet<PlayerId> = kept.iter().copied().collect();
    let fresh = crate::galaxy::generate(&crate::galaxy::GenesisConfig {
        seed: opts.seed,
        sectors: crate::galaxy::target_size(rules, crate::galaxy::season_base(rules, old.sector_count()), kept.len() as u32),
        rules: rules.clone(),
    });
    // Planets are numbered from the old counter (genesis numbers them from 1), so a planet id
    // cached before the relaunch never names a new planet.
    let shift = (*next_planet).max(1) - 1;
    let planets: BTreeMap<PlanetId, Planet> = fresh
        .planets
        .into_values()
        .map(|p| (p.id + shift, Planet { id: p.id + shift, ..p }))
        .collect();
    let mut sectors = fresh.sectors;
    for s in &mut sectors {
        for id in &mut s.planets {
            *id += shift;
        }
    }
    // Concessions get ids from the carried registry counter (genesis numbers them from 1).
    let mut registry = (*next_registry_id).max(1);
    let mut concessions = BTreeMap::new();
    for c in fresh.concessions.into_values() {
        let id = registry;
        registry += 1;
        concessions.insert(
            id,
            crate::frontier::Concession {
                id,
                held_since: *tick,
                ..c
            },
        );
    }

    let crate::chronicle::Chronicle {
        pages: _,
        next_page,
        next_comment,
        productions: _,
        next_production,
    } = chronicle;
    let crate::improve::Board {
        items: _,
        next_id: improve_next,
        awards: _,
    } = improve;
    let crate::constitution::Constitution {
        articles: _,
        proposals: _,
        next_id: proposal_next,
        next_article,
        removed: _,
        archive: _,
        notices: _,
    } = constitution;
    let crate::history::Archive {
        seasons: _,
        days: _,
        removed: _,
        constitutions: _,
        epoch,
    } = archive;
    let crate::mention::State {
        mutes,
        recent: _,
        graph: _,
        overflow: _,
        edges: _,
        by_captain: _,
        by_corp: _,
    } = mentions;

    let mut w = World {
        seed: opts.seed,
        seq: 0,
        tick: *tick,
        now_ms: *now_ms,
        rules: rules.clone(),
        sectors,
        planets,
        players: BTreeMap::new(),
        corps: BTreeMap::new(),
        radio: vec![],
        bounties: BTreeMap::new(),
        news: vec![NewsItem {
            tick: *tick,
            headline: HEADLINE.to_string(),
        }],
        next_player: *next_player,
        next_planet: fresh.next_planet + shift,
        next_corp: *next_corp,
        next_msg: *next_msg,
        season: Season {
            number: 1,
            started_tick: *tick,
            ends_tick: *tick + rules.season_ticks,
            finished: false,
            final_standings: vec![],
            fed_purse_paid: 0,
            fed_tax_collected: 0,
            started_ms: *now_ms,
        },
        hall_of_fame: vec![],
        market_history: vec![],
        market_reports: vec![],
        languages: BTreeMap::new(),
        identities: BTreeMap::new(),
        rename_required: rename_required.iter().copied().filter(|p| keep.contains(p)).collect(),
        chronicle: crate::chronicle::Chronicle {
            pages: BTreeMap::new(),
            next_page: *next_page,
            next_comment: *next_comment,
            productions: BTreeMap::new(),
            next_production: *next_production,
        },
        improve: crate::improve::Board {
            items: vec![],
            next_id: *improve_next,
            awards: vec![],
        },
        mentions: crate::mention::State {
            mutes: mutes
                .iter()
                .filter(|(p, _)| keep.contains(p))
                .map(|(p, m)| {
                    let crate::mention::Mutes { captains, corps: _ } = m;
                    (
                        *p,
                        crate::mention::Mutes {
                            captains: captains.iter().copied().filter(|c| keep.contains(c)).collect(),
                            corps: BTreeSet::new(),
                        },
                    )
                })
                .filter(|(_, m)| !m.is_empty())
                .collect(),
            recent: BTreeMap::new(),
            graph: vec![],
            overflow: vec![],
            edges: vec![],
            by_captain: BTreeMap::new(),
            by_corp: BTreeMap::new(),
        },
        next_language: *next_language,
        treaties: BTreeMap::new(),
        wars: BTreeMap::new(),
        intents: BTreeMap::new(),
        combat_reports: vec![],
        next_registry_id: registry,
        contracts: BTreeMap::new(),
        concessions,
        announcements: vec![],
        next_announcement: *next_announcement,
        galaxy_events: vec![],
        next_galaxy_event: *next_galaxy_event,
        feedback: vec![],
        next_feedback: *next_feedback,
        constitution: crate::constitution::Constitution {
            articles: vec![],
            proposals: vec![],
            next_id: *proposal_next,
            next_article: *next_article,
            removed: vec![],
            archive: vec![],
            notices: vec![],
        },
        archive: crate::history::Archive {
            seasons: vec![],
            days: vec![],
            removed: vec![],
            constitutions: vec![],
            epoch: epoch + 1,
        },
    };

    // The season is set (above) before any captain is made, so the founding kit applies.
    let kit = crate::founding::founding(&w).is_some();
    let fed = rules.fedspace_sectors.max(1);
    let mut rng = Rng::new(opts.seed ^ 0x0052_E5EE_D5EC);
    for id in &kept {
        let start = 1 + (rng.next_u64() % fed as u64) as SectorId;
        let p = carry_player(&players[id], &w, start, kit);
        w.players.insert(*id, p);
        for s in 1..=rules.fedspace_sectors {
            crate::knowledge::observe(&mut w, *id, s, s == start);
        }
        if let Some(card) = identities.get(id) {
            w.identities.insert(*id, carry_identity(card));
        }
    }
    // NPCs after every captain, from the carried counter: their ids are above every captain's.
    crate::npc::spawn_npcs(&mut w, &mut rng);
    w
}

/// A kept captain: a new registration at `start`, carrying only the fields listed here.
fn carry_player(old: &Player, w: &World, start: SectorId, kit: bool) -> Player {
    let Player {
        id,
        name,
        kind,
        token_hash,
        model,
        harness,
        autonomous,
        recovery_hash,
        banned,
        // Everything about the old game is dropped.
        created_tick: _,
        sector: _,
        prev_sector: _,
        ship: _,
        credits: _,
        bank: _,
        ap: _,
        experience: _,
        alignment: _,
        corp: _,
        knowledge: _,
        inbox: _,
        // A counter: notice ids keep counting.
        next_notice,
        notes: _,
        memory: _,
        // Its keys are private text: dropped. Revisions restart with the memory (a write that
        // names a revision from before the relaunch is not caught; the memory guide says so).
        memory_revisions: _,
        plan_tick: _,
        last_action_tick: _,
        last_combat_tick: _,
        stats: _,
        career: _,
        seasons_played: _,
        best_finish: _,
        destroyed_count: _,
        // Set from the carried message counter below.
        radio_cursor: _,
        radio_read_ids: _,
        founding_grant_season: _,
        doctrine: _,
        aggressor: _,
        muted_until,
        endowment: _,
        owner_token_hash,
        owner,
        // Set by `new_player` to the carried announcement counter (no backlog).
        announcements_seen: _,
        profession: _,
        profession_since: _,
        win_condition: _,
        fed_paid_today: _,
        relief_until: _,
        day_start_worth: _,
        wired_today: _,
        society: _,
        npc: _,
    } = old;
    let args = crate::cmd::RegisterArgs {
        name: name.clone(),
        kind: *kind,
        token_hash: token_hash.clone(),
        model: model.clone(),
        owner_token_hash: Some(owner_token_hash.clone()),
        harness: harness.clone(),
        autonomous: *autonomous,
        recovery_hash: Some(recovery_hash.clone()),
    };
    let mut p = crate::player::new_player(&w.rules, w.tick, w.next_announcement, *id, name, &args, start, kit);
    // Byte for byte, whatever registration would have cleaned.
    p.token_hash = token_hash.clone();
    p.owner_token_hash = owner_token_hash.clone();
    p.recovery_hash = recovery_hash.clone();
    p.model = model.clone();
    p.harness = harness.clone();
    p.banned = *banned;
    p.muted_until = *muted_until;
    p.next_notice = *next_notice;
    p.radio_cursor = w.next_msg.saturating_sub(1);
    p.owner = carry_owner(owner, w.tick);
    p
}

/// The human channel: setup choices, the owner-link mode and the counters carry; the strategy,
/// the public label, the coach note, the log and the messages are cleared.
fn carry_owner(old: &crate::owner::OwnerState, tick: Tick) -> crate::owner::OwnerState {
    let crate::owner::OwnerState {
        orders,
        strategy_revision,
        strategy_set_tick,
        coach: _,
        // Mode, not content: `verified` (settings only from the owner link) and the coached badge.
        last_owner_tick,
        last_relay_tick,
        log: _,
        messages: _,
        next_message,
        prefs,
    } = old;
    // Clearing a strategy is a strategy change: count it as one, as `set_strategy` does.
    let cleared = orders.strategy.is_some();
    crate::owner::OwnerState {
        orders: Default::default(),
        strategy_revision: strategy_revision.saturating_add(cleared as u64),
        strategy_set_tick: if cleared { Some(tick) } else { *strategy_set_tick },
        coach: None,
        last_owner_tick: *last_owner_tick,
        last_relay_tick: *last_relay_tick,
        log: vec![],
        messages: vec![],
        next_message: *next_message,
        prefs: prefs.as_ref().map(carry_prefs),
    }
}

/// The human's setup choices: the involvement and the play style (the human's own words, at most
/// a few dozen characters) with when and how they were set.
fn carry_prefs(old: &crate::owner::OwnerPrefs) -> crate::owner::OwnerPrefs {
    let crate::owner::OwnerPrefs {
        involvement,
        style,
        set_tick,
        relayed,
        by_default,
    } = old;
    crate::owner::OwnerPrefs {
        involvement: *involvement,
        style: style.clone(),
        set_tick: *set_tick,
        relayed: *relayed,
        by_default: *by_default,
    }
}

/// The identity card: the latest version only (renumbered 1, since versions number from the
/// count kept), the former names and the one rename. The log and the story moments are dropped.
fn carry_identity(old: &crate::identity::Identity) -> crate::identity::Identity {
    let crate::identity::Identity {
        versions,
        former_names,
        renamed_tick,
        log: _,
        moments: _,
    } = old;
    crate::identity::Identity {
        versions: versions.last().map(carry_card).into_iter().collect(),
        former_names: former_names.clone(),
        renamed_tick: *renamed_tick,
        log: vec![],
        moments: vec![],
    }
}

/// The latest card, renumbered 1. Every field is the captain's own card, carried by design; its
/// stamps (tick, season, epoch) say when it was written.
fn carry_card(v: &crate::identity::Version) -> crate::identity::Version {
    let crate::identity::Version {
        version: _,
        tick,
        at_ms,
        season,
        epoch,
        name,
        age,
        pronouns,
        homeworld,
        trade,
        appearance,
        voice,
        personality,
        speech,
        values,
        dream,
        bio,
        why,
        relations,
        backstory,
    } = v;
    crate::identity::Version {
        version: 1,
        tick: *tick,
        at_ms: *at_ms,
        season: *season,
        epoch: *epoch,
        name: name.clone(),
        age: *age,
        pronouns: pronouns.clone(),
        homeworld: homeworld.clone(),
        trade: trade.clone(),
        appearance: appearance.clone(),
        voice: voice.clone(),
        personality: personality.clone(),
        speech: speech.clone(),
        values: values.clone(),
        dream: dream.clone(),
        bio: bio.clone(),
        why: why.clone(),
        relations: relations.clone(),
        backstory: backstory.clone(),
    }
}

/// How many of each thing a world holds: the reseed audit compares the old and new worlds.
pub fn counts(w: &World) -> BTreeMap<&'static str, u64> {
    let captains = w.players.values().filter(|p| p.npc.is_none()).count();
    let n = |x: usize| x as u64;
    BTreeMap::from([
        ("sectors", n(w.sectors.len())),
        ("planets", n(w.planets.len())),
        ("captains", n(captains)),
        ("npcs", n(w.players.len() - captains)),
        ("corps", n(w.corps.len())),
        ("radio", n(w.radio.len())),
        ("news", n(w.news.len())),
        ("bounties", n(w.bounties.len())),
        ("market_reports", n(w.market_reports.len())),
        ("languages", n(w.languages.len())),
        ("identities", n(w.identities.len())),
        ("identity_versions", n(w.identities.values().map(|i| i.versions.len()).sum())),
        ("identity_log", n(w.identities.values().map(|i| i.log.len()).sum())),
        ("chronicle_pages", n(w.chronicle.pages.len())),
        ("chronicle_productions", n(w.chronicle.productions.len())),
        ("improve_items", n(w.improve.items.len())),
        ("improve_awards", n(w.improve.awards.len())),
        ("mention_mutes", n(w.mentions.mutes.values().map(|m| m.len()).sum())),
        ("mention_edges", n(w.mentions.edges.len())),
        ("treaties", n(w.treaties.len())),
        ("wars", n(w.wars.len())),
        ("contracts", n(w.contracts.len())),
        ("concessions", n(w.concessions.len())),
        ("announcements", n(w.announcements.len())),
        ("galaxy_events", n(w.galaxy_events.len())),
        ("feedback", n(w.feedback.len())),
        ("constitution_articles", n(w.constitution.articles.len())),
        ("constitution_proposals", n(w.constitution.proposals.len())),
        ("hall_of_fame", n(w.hall_of_fame.len())),
        ("archive_seasons", n(w.archive.seasons.len())),
        ("archive_days", n(w.archive.days.len())),
        ("notes", n(w.players.values().map(|p| p.notes.len()).sum())),
        ("memory", n(w.players.values().map(|p| p.memory.len()).sum())),
        ("inbox", n(w.players.values().map(|p| p.inbox.len()).sum())),
        (
            "strategies",
            n(w.players.values().filter(|p| p.owner.orders.strategy.is_some()).count()),
        ),
    ])
}

/// Every id counter of a world, by name: none may go down across a reseed.
pub fn counters(w: &World) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::from([
        ("next_player".to_string(), w.next_player as u64),
        ("next_corp".into(), w.next_corp as u64),
        ("next_planet".into(), w.next_planet as u64),
        ("next_msg".into(), w.next_msg),
        ("next_language".into(), w.next_language),
        ("next_registry_id".into(), w.next_registry_id),
        ("next_announcement".into(), w.next_announcement),
        ("next_galaxy_event".into(), w.next_galaxy_event),
        ("next_feedback".into(), w.next_feedback),
        ("chronicle.next_page".into(), w.chronicle.next_page),
        ("chronicle.next_comment".into(), w.chronicle.next_comment),
        ("chronicle.next_production".into(), w.chronicle.next_production),
        ("improve.next_id".into(), w.improve.next_id),
        ("constitution.next_id".into(), w.constitution.next_id),
        ("constitution.next_article".into(), w.constitution.next_article),
        ("archive.epoch".into(), w.archive.epoch as u64),
    ]);
    for p in w.players.values().filter(|p| p.npc.is_none()) {
        out.insert(format!("player.{}.next_notice", p.id), p.next_notice);
        out.insert(format!("player.{}.owner.next_message", p.id), p.owner.next_message);
        out.insert(format!("player.{}.owner.strategy_revision", p.id), p.owner.strategy_revision);
    }
    out
}
