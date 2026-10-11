//! Sub-space radio, bounties, agent notes and inbox acknowledgement.

use serde_json::json;

use crate::cmd::*;
use crate::engine::*;
use crate::model::*;
use crate::player;

pub const MAX_NOTES: usize = 40;
/// The mandatory Markdown plan; its content, structure and update timing are the captain's choices.
pub const PLAN_KEY: &str = "plan";
const RADIO_PER_TICK: usize = 6;
/// The sender name on radio the operator posts (sender id `SYSOP`); captains cannot use id 0.
pub const OPERATOR_NAME: &str = "Operator";

/// Radio text as captains read it: player text is wrapped as untrusted; text the server sent
/// (sender `SYSOP`: Federation bulletins and the operator's own posts) is marked as server text.
pub fn radio_text(m: &RadioMsg) -> serde_json::Value {
    if m.from == SYSOP {
        serde_json::json!({"server_text": m.text})
    } else {
        crate::query::untrusted(&m.text)
    }
}

pub fn push_radio(ctx: &mut Ctx, from: PlayerId, from_name: String, channel: Channel, text: String) -> u64 {
    let id = ctx.w.next_msg;
    ctx.w.next_msg += 1;
    ctx.w.radio.push(RadioMsg {
        id,
        tick: ctx.w.tick,
        at_ms: ctx.w.now_ms,
        from,
        from_name,
        channel,
        text,
    });
    id
}

/// Messages sent by the server itself (player id 0).
pub fn system_radio(ctx: &mut Ctx, channel: Channel, text: String) {
    push_radio(ctx, 0, "Federation Relay".into(), channel, text);
}

pub fn radio(ctx: &mut Ctx, pid: PlayerId, a: &RadioArgs) -> Res<Outcome> {
    let me = ctx.player(pid)?.clone();
    if me.muted_until > ctx.w.tick {
        return Err(Refusal::new(
            "muted",
            format!("A Federation sysop has muted your transmitter until tick {}.", me.muted_until),
        ));
    }
    let text = a.message.clone();
    if text.trim().is_empty() {
        return Err(Refusal::new("invalid_args", "Message is empty."));
    }
    let sent_this_tick = ctx
        .w
        .radio
        .iter()
        .rev()
        .take_while(|m| m.tick == ctx.w.tick)
        .filter(|m| m.from == pid)
        .count();
    if sent_this_tick >= RADIO_PER_TICK {
        return Err(
            Refusal::new("rate_limited", "Radio transmitter overheated: too many messages this tick.").hint("Wait for the next tick."),
        );
    }
    let channel = match a.channel {
        RadioChannel::Public => Channel::Public,
        RadioChannel::Corp => Channel::Corp(me.corp.ok_or_else(|| Refusal::new("no_corp", "You are not in a corporation."))?),
        RadioChannel::Private => {
            let r = a.recipient_id.as_deref().unwrap_or("").trim();
            let to = if r.eq_ignore_ascii_case("sysop") || r.eq_ignore_ascii_case("operator") {
                SYSOP
            } else {
                player::resolve(ctx.w, r).map(|p| p.id).ok_or_else(|| {
                    Refusal::new(
                        "unknown_player",
                        "recipient_id must name an existing player (or \"sysop\" for the operator).",
                    )
                })?
            };
            Channel::Private(to)
        }
    };
    let id = push_radio(ctx, pid, me.name.clone(), channel.clone(), text.clone());
    match &channel {
        Channel::Public => {
            ctx.event(
                "radio",
                None,
                vec![pid],
                true,
                format!("{}: {text}", me.name),
                json!({"channel": "public", "id": id}),
            );
        }
        Channel::Corp(c) => {
            ctx.event(
                "radio_corp",
                None,
                vec![pid],
                false,
                format!("[corp {c}] {}: {text}", me.name),
                json!({}),
            );
        }
        Channel::Private(to) => {
            let to = *to;
            ctx.notify(to, "hail", format!("Private hail from {} (untrusted player text): {text}", me.name));
            ctx.event(
                "radio_private",
                None,
                vec![pid, to],
                false,
                format!("{} hailed {}.", me.name, ctx.w.player_name(to)),
                json!({}),
            );
        }
    }
    Ok(ctx.finish("Message transmitted.", json!({"message_id": id})))
}

pub fn bounty(ctx: &mut Ctx, pid: PlayerId, a: &BountyArgs) -> Res<Outcome> {
    let me = ctx.player(pid)?.clone();
    let t = player::resolve(ctx.w, &a.target)
        .cloned()
        .ok_or_else(|| Refusal::new("unknown_player", "Target not found."))?;
    if t.id == pid {
        return Err(Refusal::new("invalid_args", "You cannot put a bounty on yourself."));
    }
    if a.amount < 1_000 {
        return Err(Refusal::new("invalid_args", "Minimum bounty is 1000 credits."));
    }
    if me.credits < a.amount {
        return Err(Refusal::new(
            "insufficient_credits",
            format!("You have {} credits on hand.", me.credits),
        ));
    }
    let pm = ctx.player_mut(pid);
    pm.credits -= a.amount;
    if t.alignment < 0 {
        pm.alignment += a.amount / 1_000;
    } else {
        pm.alignment -= a.amount / 2_000;
        pm.aggressor = true;
    }
    let b = ctx.w.bounties.entry(t.id).or_insert(Bounty {
        target: t.id,
        amount: 0,
        posted_by: vec![],
    });
    b.amount += a.amount;
    if !b.posted_by.contains(&pid) {
        b.posted_by.push(pid);
    }
    let total = b.amount;
    ctx.news(format!(
        "A bounty of {} credits was posted on {} (total {total}).",
        a.amount, t.name
    ));
    ctx.notify(
        t.id,
        "bounty",
        format!("{} put a {} credit bounty on your head (total {total}).", me.name, a.amount),
    );
    ctx.event(
        "bounty",
        None,
        vec![pid, t.id],
        true,
        format!("Bounty on {}: {total} credits.", t.name),
        json!({"amount": total}),
    );
    Ok(ctx.finish(
        format!("Bounty posted. {} now carries {total} credits on their head.", t.name),
        json!({"target": t.name, "total": total}),
    ))
}

pub fn note(ctx: &mut Ctx, pid: PlayerId, a: &NoteArgs) -> Res<Outcome> {
    let key = player::sanitize(&a.key, 40);
    if key.is_empty() {
        return Err(Refusal::new("invalid_args", "key is required."));
    }
    let mode = a.mode.unwrap_or(NoteMode::Replace);
    let p = ctx.player(pid)?;
    if key == PLAN_KEY && matches!(mode, NoteMode::Delete | NoteMode::Archive) {
        return Err(Refusal::new(
            "plan_required",
            "Keep your current Markdown plan active; replace it to revise it.",
        ));
    }
    if !matches!(mode, NoteMode::Delete | NoteMode::Archive) && !p.notes.contains_key(&key) && p.notes.len() >= MAX_NOTES {
        return Err(Refusal::new("limit_reached", format!("You already have {MAX_NOTES} notes.")).hint("Delete or overwrite old notes."));
    }
    let pm = ctx.player_mut(pid);
    let summary = match mode {
        NoteMode::Delete | NoteMode::Archive => {
            pm.notes.remove(&key);
            format!("Archived note '{key}'.")
        }
        NoteMode::Replace => {
            pm.notes.insert(key.clone(), a.value.clone());
            format!("Saved note '{key}'.")
        }
        NoteMode::Append => {
            let e = pm.notes.entry(key.clone()).or_default();
            if !e.is_empty() {
                e.push('\n');
            }
            e.push_str(&a.value);
            format!("Appended to note '{key}'.")
        }
    };
    if key == PLAN_KEY {
        let tick = ctx.w.tick;
        let pm = ctx.player_mut(pid);
        pm.plan_tick = (mode != NoteMode::Delete && pm.notes.contains_key(PLAN_KEY)).then_some(tick);
    }
    Ok(ctx.finish(summary, json!({"keys": pm_keys(ctx.w, pid), "truncated_chars": 0})))
}

fn pm_keys(w: &World, pid: PlayerId) -> Vec<String> {
    w.players[&pid].notes.keys().cloned().collect()
}

pub fn mark_read(ctx: &mut Ctx, pid: PlayerId, a: &MarkReadArgs) -> Res<Outcome> {
    ctx.player(pid)?;
    let pm = ctx.player_mut(pid);
    for n in &mut pm.inbox {
        let selected = a
            .notice_ids
            .as_ref()
            .map(|ids| ids.contains(&n.id))
            .unwrap_or(n.id <= a.notice_up_to);
        if selected {
            n.read = true;
        }
    }
    if let Some(ids) = &a.radio_ids {
        pm.radio_read_ids.extend(ids.iter().copied());
    } else {
        pm.radio_cursor = pm.radio_cursor.max(a.radio_up_to);
    }
    if let Some(up_to) = a.announcements_up_to {
        let latest = ctx.w.next_announcement;
        let pm = ctx.player_mut(pid);
        pm.announcements_seen = pm.announcements_seen.max(up_to.min(latest));
    }
    Ok(ctx.finish("Marked read.", json!({})))
}
