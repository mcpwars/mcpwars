//! Corporations: shared treasury, shared starchart, mutual recognition.

use serde_json::json;

use crate::cmd::*;
use crate::engine::*;
use crate::government;
use crate::model::*;
use crate::player;
use crate::query;

fn corp_of(w: &World, pid: PlayerId) -> Res<Corp> {
    w.players[&pid].corp.and_then(|c| w.corps.get(&c)).cloned().ok_or_else(|| {
        Refusal::new("no_corp", "You are not in a corporation.").hint("corp action=create name=... or accept an invite with action=join.")
    })
}

fn require_ceo(c: &Corp, pid: PlayerId) -> Res<()> {
    if c.ceo != pid {
        return Err(Refusal::new("permission_denied", "Only the CEO can do that."));
    }
    Ok(())
}

fn target(w: &World, a: &CorpArgs) -> Res<PlayerId> {
    a.player
        .as_deref()
        .and_then(|s| player::resolve(w, s))
        .map(|p| p.id)
        .ok_or_else(|| Refusal::new("unknown_player", "player (name or id) is required and must exist."))
}

pub fn corp(ctx: &mut Ctx, pid: PlayerId, a: &CorpArgs) -> Res<Outcome> {
    let me = ctx.player(pid)?.clone();
    let summary: String;
    match a.action {
        CorpAction::Create => {
            if me.corp.is_some() {
                return Err(Refusal::new("already_in_corp", "Leave your current corporation first."));
            }
            let name = valid_corp_name(ctx.w, a.name.as_deref().unwrap_or(""), &[])?;
            let fee = crate::founding::charter_fee(ctx.w);
            if me.credits < fee {
                return Err(ctx.no_credits(pid, "A corporate charter", fee));
            }
            let id = ctx.w.next_corp;
            ctx.w.next_corp += 1;
            let motto = a.motto.clone().unwrap_or_default();
            ctx.w.corps.insert(
                id,
                Corp {
                    id,
                    name: name.clone(),
                    ceo: pid,
                    members: vec![pid],
                    invites: vec![],
                    treasury: 0,
                    motto,
                    created_tick: ctx.w.tick,
                    deposits: Default::default(),
                    record: Default::default(),
                    gov: Default::default(),
                    seat: None,
                    seat_set_tick: None,
                },
            );
            let pm = ctx.player_mut(pid);
            pm.credits -= fee;
            pm.corp = Some(id);
            ctx.news(format!("{} founded the corporation {name}.", me.name));
            ctx.event(
                "corp_created",
                None,
                vec![pid],
                true,
                format!("{} founded {name}.", me.name),
                json!({"corp": id}),
            );
            summary = format!("Founded {name}. Invite members with corp action=invite player=<name>.");
        }
        CorpAction::Invite => {
            let c = corp_of(ctx.w, pid)?;
            require_ceo(&c, pid)?;
            let t = target(ctx.w, a)?;
            if ctx.w.players[&t].corp.is_some() {
                return Err(Refusal::new("already_in_corp", "That player is already in a corporation."));
            }
            if !c.invites.contains(&t) {
                ctx.w.corps.get_mut(&c.id).unwrap().invites.push(t);
            }
            ctx.notify(
                t,
                "corp_invite",
                format!(
                    "{} invites you to join the corporation {}. Accept with corp action=join name=\"{}\".",
                    me.name, c.name, c.name
                ),
            );
            summary = format!("Invited {} to {}.", ctx.w.player_name(t), c.name);
        }
        CorpAction::Join => {
            if me.corp.is_some() {
                return Err(Refusal::new("already_in_corp", "Leave your current corporation first."));
            }
            let name = a.name.clone().unwrap_or_default();
            let c = ctx
                .w
                .corps
                .values()
                .find(|c| c.name.eq_ignore_ascii_case(name.trim()) || c.invites.contains(&pid) && name.trim().is_empty())
                .cloned()
                .ok_or_else(|| Refusal::new("unknown_corp", "No such corporation."))?;
            if !c.invites.contains(&pid) {
                return Err(Refusal::new("not_invited", format!("You have no invitation from {}.", c.name)).hint("Ask its CEO on radio."));
            }
            let cm = ctx.w.corps.get_mut(&c.id).unwrap();
            cm.invites.retain(|x| *x != pid);
            cm.members.push(pid);
            ctx.player_mut(pid).corp = Some(c.id);
            ctx.notify(c.ceo, "corp", format!("{} joined {}.", me.name, c.name));
            ctx.news(format!("{} joined the corporation {}.", me.name, c.name));
            crate::founding::pay_grants(ctx);
            summary = format!("Joined {}. Your starcharts are now shared with your corp-mates.", c.name);
        }
        CorpAction::Leave => {
            let c = corp_of(ctx.w, pid)?;
            leave(ctx, pid, &c);
            summary = format!("Left {}.", c.name);
        }
        CorpAction::Kick => {
            let c = corp_of(ctx.w, pid)?;
            require_ceo(&c, pid)?;
            government::republic_refusal(&c, "remove a member", "government action=remove player=<name> opens the measure.")?;
            let t = target(ctx.w, a)?;
            if t == pid || !c.members.contains(&t) {
                return Err(Refusal::new("invalid_args", "That player is not a member you can remove."));
            }
            leave(ctx, t, &c);
            ctx.notify(t, "corp", format!("You were removed from {}.", c.name));
            summary = format!("Removed {} from {}.", ctx.w.player_name(t), c.name);
            government::record_executive(
                ctx,
                c.id,
                "remove",
                format!("{} removed {} by decree.", me.name, ctx.w.player_name(t)),
                pid,
                0,
            );
        }
        CorpAction::Deposit => {
            let c = corp_of(ctx.w, pid)?;
            let n = a.amount.unwrap_or(me.credits).min(me.credits);
            if n <= 0 {
                return Err(Refusal::new("invalid_args", "Nothing to deposit."));
            }
            ctx.player_mut(pid).credits -= n;
            let cm = ctx.w.corps.get_mut(&c.id).unwrap();
            cm.treasury += n;
            *cm.deposits.entry(pid).or_insert(0) += n;
            let held = cm.deposit_of(pid);
            summary = format!(
                "Deposited {n} into the {} treasury. Your deposit of {held} is reserved for you: you may withdraw it at any time.",
                c.name
            );
        }
        CorpAction::Withdraw => {
            // Members withdraw their own reserved deposit; the CEO may also take the free funds
            // beyond everyone's deposits, and the other members are told when it does.
            let c = corp_of(ctx.w, pid)?;
            let own = c.deposit_of(pid).min(c.treasury);
            // A republic's free funds are spent only by the members' vote (government action=grant).
            let free = if c.ceo == pid && c.gov.form == government::Form::Autocracy {
                c.free_funds()
            } else {
                0
            };
            let limit = own + free;
            if limit <= 0 {
                return Err(if c.ceo == pid && c.gov.form == government::Form::Republic {
                    Refusal::new(
                        "republic",
                        format!("{} is a republic: its free funds are spent by the members' vote.", c.name),
                    )
                    .hint("government action=grant player=<name> amount=<n> opens the measure.")
                } else if c.ceo == pid {
                    Refusal::new("invalid_args", "Nothing to withdraw: the treasury holds only members' deposits.")
                } else {
                    Refusal::new("invalid_args", "You have no deposit to withdraw.")
                        .hint("Members withdraw their own deposits; the CEO manages the rest.")
                });
            }
            let n = a.amount.unwrap_or(limit);
            if n <= 0 {
                return Err(Refusal::new("invalid_args", "amount must be positive."));
            }
            if n > limit {
                return Err(Refusal::new(
                    "over_limit",
                    format!("You may withdraw up to {limit} (your deposit {own}, free funds you control {free})."),
                ));
            }
            let from_own = n.min(own);
            let from_free = n - from_own;
            let cm = ctx.w.corps.get_mut(&c.id).unwrap();
            cm.treasury -= n;
            if from_own > 0 {
                *cm.deposits.get_mut(&pid).unwrap() -= from_own;
                cm.deposits.retain(|_, v| *v > 0);
            }
            ctx.player_mut(pid).credits += n;
            if from_free > 0 {
                government::record_executive(
                    ctx,
                    c.id,
                    "withdraw",
                    format!("{} (CEO) withdrew {from_free} of free funds by decree.", me.name),
                    pid,
                    from_free,
                );
                for m in c.members.iter().filter(|m| **m != pid) {
                    ctx.notify(
                        *m,
                        "corp",
                        format!("{} (CEO) withdrew {from_free} of free funds from the {} treasury.", me.name, c.name),
                    );
                }
            }
            summary = format!("Withdrew {n} from the {} treasury.", c.name);
        }
        CorpAction::TransferCeo => {
            let c = corp_of(ctx.w, pid)?;
            require_ceo(&c, pid)?;
            government::republic_refusal(
                &c,
                "elect their CEO",
                "The CEO changes at an election: each term, or after a recall (government action=recall).",
            )?;
            let t = target(ctx.w, a)?;
            if !c.members.contains(&t) {
                return Err(Refusal::new("invalid_args", "The new CEO must be a member."));
            }
            ctx.w.corps.get_mut(&c.id).unwrap().ceo = t;
            ctx.notify(t, "corp", format!("You are now CEO of {}.", c.name));
            summary = format!("{} is now CEO of {}.", ctx.w.player_name(t), c.name);
            government::record_executive(
                ctx,
                c.id,
                "ceo",
                format!("{} handed the CEO seat to {} by decree.", me.name, ctx.w.player_name(t)),
                pid,
                0,
            );
        }
        CorpAction::SetMotto => {
            let c = corp_of(ctx.w, pid)?;
            require_ceo(&c, pid)?;
            let m = a.motto.clone().unwrap_or_default();
            ctx.w.corps.get_mut(&c.id).unwrap().motto = m;
            summary = "Motto updated.".into();
        }
    }
    let reply = json!({"corp": query::corp_view(ctx.w, pid), "you": brief(ctx.w, pid)});
    Ok(ctx.finish(summary, reply))
}

/// Remove `pid` from corp `c`, paying back its deposit and handing over or dissolving as needed.
pub fn leave(ctx: &mut Ctx, pid: PlayerId, c: &Corp) {
    ctx.player_mut(pid).corp = None;
    let cm = ctx.w.corps.get_mut(&c.id).unwrap();
    cm.members.retain(|m| *m != pid);
    let refund = cm.deposits.remove(&pid).unwrap_or(0).min(cm.treasury);
    cm.treasury -= refund;
    ctx.player_mut(pid).credits += refund;
    if refund > 0 {
        ctx.notify(
            pid,
            "corp",
            format!("Your deposit of {refund} was returned from the {} treasury.", c.name),
        );
    }
    let cm = ctx.w.corps.get_mut(&c.id).unwrap();
    if cm.members.is_empty() {
        let treasury = cm.treasury;
        ctx.w.corps.remove(&c.id);
        ctx.player_mut(pid).credits += treasury;
        ctx.news(format!("The corporation {} has dissolved.", c.name));
    } else if cm.ceo == pid {
        cm.ceo = cm.members[0];
        let new_ceo = cm.ceo;
        ctx.notify(new_ceo, "corp", format!("You are now CEO of {}.", c.name));
    }
}

fn corp_by_id(w: &World, id: CorpId) -> Res<Corp> {
    w.corps
        .get(&id)
        .cloned()
        .ok_or_else(|| Refusal::new("unknown_corp", format!("No corporation #{id}.")))
}

fn valid_corp_name(w: &World, name: &str, except: &[CorpId]) -> Res<String> {
    let name = player::sanitize(name, 24);
    if !player::valid_name(&name) {
        return Err(Refusal::new("invalid_name", "Corporation names are 2-24 letters/digits/spaces."));
    }
    // Engine 1.58: names are unique by `name_key` (case and punctuation ignored), and a
    // corporation may not take a captain's current or former call sign, so mentions resolve.
    let key = player::name_key(&name);
    if w.corps
        .values()
        .any(|c| !except.contains(&c.id) && player::name_key(&c.name) == key)
    {
        return Err(Refusal::new("name_taken", "A corporation with that name exists."));
    }
    if crate::identity::captain_named(w, &name).is_some() {
        return Err(Refusal::new(
            "name_taken",
            "A captain uses or used that call sign; choose another corporation name.",
        ));
    }
    Ok(name)
}

/// The operator merges `from` into `into` (engine 1.60; see `AdminArgs::MergeCorps`). Everything is
/// checked before anything changes. Returns the summary and the merged corporation's id.
fn merge(ctx: &mut Ctx, from: CorpId, into: CorpId, message: &str, name: Option<&str>, ceo: Option<PlayerId>) -> Res<(String, CorpId)> {
    if from == into {
        return Err(Refusal::new("invalid_args", "A corporation cannot merge into itself."));
    }
    let a = corp_by_id(ctx.w, from)?;
    let b = corp_by_id(ctx.w, into)?;
    let message = message.replace('\0', "");
    if message.trim().is_empty() {
        return Err(Refusal::new(
            "invalid_args",
            "Explain the merger in message: every member of both corporations receives it.",
        ));
    }
    if message.chars().count() > crate::constitution::OPERATOR_TEXT_MAX {
        return Err(Refusal::new(
            "too_long",
            format!("The message holds at most {} characters.", crate::constitution::OPERATOR_TEXT_MAX),
        ));
    }
    let new_name = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) if n != b.name => Some(valid_corp_name(ctx.w, n, &[from, into])?),
        _ => None,
    };
    if let Some(c) = ceo
        && !a.members.contains(&c)
        && !b.members.contains(&c)
    {
        return Err(Refusal::new(
            "invalid_args",
            "The CEO must be a member of one of the two corporations.",
        ));
    }
    // Members join `into`; invitations are pooled.
    for m in &a.members {
        ctx.player_mut(*m).corp = Some(into);
    }
    let bm = ctx.w.corps.get_mut(&into).unwrap();
    bm.members.extend(a.members.iter().copied());
    for i in &a.invites {
        if !bm.invites.contains(i) {
            bm.invites.push(*i);
        }
    }
    let all = bm.members.clone();
    bm.invites.retain(|i| !all.contains(i));
    // Treaties and wars, while `from` still holds its treasury for returned bonds.
    let d = crate::diplomacy::merge_corps(ctx, from, into);
    let a = ctx.w.corps.remove(&from).unwrap();
    let bm = ctx.w.corps.get_mut(&into).unwrap();
    bm.treasury += a.treasury;
    for (p, v) in &a.deposits {
        *bm.deposits.entry(*p).or_insert(0) += v;
    }
    bm.record.absorb(&a.record);
    bm.gov.founding_grant_season = bm.gov.founding_grant_season.max(a.gov.founding_grant_season);
    if bm.seat.is_none() {
        bm.seat = a.seat;
        bm.seat_set_tick = a.seat_set_tick;
    }
    let old_name = bm.name.clone();
    if let Some(n) = &new_name {
        bm.name = n.clone();
    }
    let new_ceo = ceo.filter(|c| *c != bm.ceo);
    if let Some(c) = new_ceo {
        bm.ceo = c;
    }
    let (merged, treasury, members) = (bm.name.clone(), a.treasury, a.members.len());
    if let Some(c) = new_ceo {
        ctx.notify(c, "corp", format!("The Federation made you CEO of {merged}."));
    }
    // Market reports shared with `from` are now shared with the merged corporation.
    for r in ctx.w.market_reports.iter_mut().filter(|r| r.corp == Some(from)) {
        r.corp = Some(into);
    }
    let renamed = new_name.as_ref().map(|n| format!(", now named {n}")).unwrap_or_default();
    let fact = format!("The Federation merged {} into {old_name}{renamed}.", a.name);
    government::log_decision(
        ctx,
        into,
        "merge",
        format!("{fact} {members} member(s) and a treasury of {treasury} joined."),
        SYSOP,
        None,
        0,
        0,
        treasury,
    );
    for m in &a.members {
        let deposit = a.deposits.get(m).copied().unwrap_or(0);
        let kept = if deposit > 0 {
            format!(" Your deposit of {deposit} carried over.")
        } else {
            String::new()
        };
        ctx.notify(*m, "corp", format!("{fact} You are now a member of {merged}.{kept}"));
    }
    for m in &b.members {
        ctx.notify(
            *m,
            "corp",
            format!("{fact} Its {members} member(s) and treasury of {treasury} joined {merged}."),
        );
    }
    // The operator's explanation, trusted, to every member of both.
    let everyone: Vec<PlayerId> = b.members.iter().chain(a.members.iter()).copied().collect();
    for m in &everyone {
        ctx.notify(*m, "sysop", message.clone());
    }
    ctx.news(format!("The corporation {} merged into {old_name}{renamed}.", a.name));
    ctx.event(
        "corp_merged",
        None,
        everyone,
        true,
        format!("{} merged into {merged}.", a.name),
        json!({"from": from, "into": into}),
    );
    let mut s = format!(
        "Merged {} into {merged}: {members} member(s) and a treasury of {treasury} joined",
        a.name
    );
    if d.treaties_moved + d.wars_moved > 0 {
        s.push_str(&format!(
            "; {} treaty(ies) and {} war(s) passed to it",
            d.treaties_moved, d.wars_moved
        ));
    }
    if d.treaties_ended + d.wars_ended > 0 {
        s.push_str(&format!("; {} treaty(ies) and {} war(s) ended", d.treaties_ended, d.wars_ended));
    }
    s.push_str(&format!("; {} captain(s) told.", b.members.len() + a.members.len()));
    Ok((s, into))
}

/// The operator's corporation commands. Membership changes follow the game's own rules
/// (`leave` hands over the CEO seat or dissolves an empty corporation).
pub fn admin(ctx: &mut Ctx, a: &AdminArgs) -> Res<Outcome> {
    let summary: String;
    let mut corp_id: Option<CorpId> = None;
    match a {
        AdminArgs::CreateCorp { name, ceo, motto } => {
            let p = ctx.player(*ceo)?.clone();
            if p.npc.is_some() {
                return Err(Refusal::new("invalid_args", "NPCs cannot run corporations."));
            }
            if p.corp.is_some() {
                return Err(Refusal::new("already_in_corp", format!("{} is already in a corporation.", p.name)));
            }
            let name = valid_corp_name(ctx.w, name, &[])?;
            let id = ctx.w.next_corp;
            ctx.w.next_corp += 1;
            let motto = motto.clone().unwrap_or_default();
            ctx.w.corps.insert(
                id,
                Corp {
                    id,
                    name: name.clone(),
                    ceo: *ceo,
                    members: vec![*ceo],
                    invites: vec![],
                    treasury: 0,
                    motto,
                    created_tick: ctx.w.tick,
                    deposits: Default::default(),
                    record: Default::default(),
                    gov: Default::default(),
                    seat: None,
                    seat_set_tick: None,
                },
            );
            for c in ctx.w.corps.values_mut() {
                c.invites.retain(|x| x != ceo);
            }
            ctx.player_mut(*ceo).corp = Some(id);
            ctx.notify(*ceo, "corp", format!("The Federation chartered {name} for you; you are its CEO."));
            ctx.news(format!("{} founded the corporation {name}.", p.name));
            ctx.event(
                "corp_created",
                None,
                vec![*ceo],
                true,
                format!("{} founded {name}.", p.name),
                json!({"corp": id}),
            );
            corp_id = Some(id);
            summary = format!("Chartered {name} with {} as CEO.", p.name);
        }
        AdminArgs::SetCorp {
            corp,
            name,
            motto,
            ceo,
            treasury,
        } => {
            let c = corp_by_id(ctx.w, *corp)?;
            let mut changed = vec![];
            let new_name = match name {
                Some(n) if n.trim() != c.name => Some(valid_corp_name(ctx.w, n, &[c.id])?),
                _ => None,
            };
            if let Some(t) = ceo
                && !c.members.contains(t)
            {
                return Err(Refusal::new("invalid_args", "The CEO must be a member."));
            }
            let cm = ctx.w.corps.get_mut(corp).unwrap();
            if let Some(n) = &new_name {
                cm.name = n.clone();
                changed.push(format!("name={n}"));
            }
            if let Some(m) = motto {
                cm.motto = m.clone();
                changed.push("motto".to_string());
            }
            if let Some(v) = treasury {
                cm.treasury = (*v).max(0);
                // Deposits never exceed the treasury: a cut below them reduces every deposit in proportion.
                let t = cm.treasury;
                scale_down(&mut cm.deposits, t);
                cm.deposits.retain(|_, v| *v > 0);
                changed.push(format!("treasury={}", cm.treasury));
            }
            if let Some(t) = ceo
                && cm.ceo != *t
            {
                cm.ceo = *t;
                changed.push(format!("ceo={}", ctx.w.player_name(*t)));
                ctx.notify(
                    *t,
                    "corp",
                    format!("The Federation made you CEO of {}.", new_name.as_deref().unwrap_or(&c.name)),
                );
            }
            if changed.is_empty() {
                return Err(Refusal::new("invalid_args", "Nothing to change."));
            }
            if let Some(n) = &new_name {
                ctx.news(format!("The corporation {} is now {n}.", c.name));
            }
            corp_id = Some(c.id);
            summary = format!("Sysop updated {}: {}.", new_name.as_deref().unwrap_or(&c.name), changed.join(", "));
        }
        AdminArgs::CorpAddMember { corp, player: pid } => {
            let c = corp_by_id(ctx.w, *corp)?;
            let p = ctx.player(*pid)?.clone();
            if p.npc.is_some() {
                return Err(Refusal::new("invalid_args", "NPCs cannot join corporations."));
            }
            if c.members.contains(pid) {
                return Err(Refusal::new("invalid_args", format!("{} is already a member.", p.name)));
            }
            if let Some(old) = p.corp.and_then(|o| ctx.w.corps.get(&o)).cloned() {
                leave(ctx, *pid, &old);
            }
            let cm = ctx.w.corps.get_mut(corp).unwrap();
            cm.invites.retain(|x| x != pid);
            cm.members.push(*pid);
            ctx.player_mut(*pid).corp = Some(c.id);
            ctx.notify(*pid, "corp", format!("The Federation placed you in {}.", c.name));
            ctx.news(format!("{} joined the corporation {}.", p.name, c.name));
            corp_id = Some(c.id);
            summary = format!("{} is now a member of {}.", p.name, c.name);
        }
        AdminArgs::CorpRemoveMember { corp, player: pid } => {
            let c = corp_by_id(ctx.w, *corp)?;
            if !c.members.contains(pid) {
                return Err(Refusal::new("invalid_args", "That captain is not a member."));
            }
            let name = ctx.w.player_name(*pid);
            leave(ctx, *pid, &c);
            ctx.notify(*pid, "corp", format!("The Federation removed you from {}.", c.name));
            corp_id = ctx.w.corps.contains_key(corp).then_some(c.id);
            summary = if ctx.w.corps.contains_key(corp) {
                format!("Removed {name} from {}.", c.name)
            } else {
                format!("Removed {name}; {} had no members left and dissolved.", c.name)
            };
        }
        AdminArgs::CorpInvite { corp, player: pid } => {
            let c = corp_by_id(ctx.w, *corp)?;
            let p = ctx.player(*pid)?.clone();
            if p.corp.is_some() {
                return Err(Refusal::new("already_in_corp", format!("{} is already in a corporation.", p.name)));
            }
            if !c.invites.contains(pid) {
                ctx.w.corps.get_mut(corp).unwrap().invites.push(*pid);
            }
            ctx.notify(
                *pid,
                "corp_invite",
                format!(
                    "You are invited to join the corporation {}. Accept with corp action=join name=\"{}\".",
                    c.name, c.name
                ),
            );
            corp_id = Some(c.id);
            summary = format!("Invited {} to {}.", p.name, c.name);
        }
        AdminArgs::CorpUninvite { corp, player: pid } => {
            let c = corp_by_id(ctx.w, *corp)?;
            if !c.invites.contains(pid) {
                return Err(Refusal::new("invalid_args", "That captain has no invitation."));
            }
            ctx.w.corps.get_mut(corp).unwrap().invites.retain(|x| x != pid);
            corp_id = Some(c.id);
            summary = format!("Withdrew {}'s invitation to {}.", ctx.w.player_name(*pid), c.name);
        }
        AdminArgs::DisbandCorp { corp } => {
            let c = corp_by_id(ctx.w, *corp)?;
            let mut order: Vec<PlayerId> = c.members.iter().copied().filter(|m| *m != c.ceo).collect();
            order.push(c.ceo);
            for m in order {
                if let Some(cur) = ctx.w.corps.get(corp).cloned() {
                    leave(ctx, m, &cur);
                    ctx.notify(m, "corp", format!("The Federation disbanded {}.", c.name));
                }
            }
            ctx.w.corps.remove(corp);
            summary = format!(
                "Disbanded {}; members were paid their deposits and the rest of its treasury of {} went to {}.",
                c.name,
                c.treasury,
                ctx.w.player_name(c.ceo)
            );
        }
        AdminArgs::MergeCorps {
            from,
            into,
            message,
            name,
            ceo,
        } => {
            let (summary_text, id) = merge(ctx, *from, *into, message, name.as_deref(), *ceo)?;
            summary = summary_text;
            corp_id = Some(id);
        }
        _ => return Err(Refusal::new("invalid_args", "Not a corporation command.")),
    }
    let reply = json!({"corp": corp_id.and_then(|id| ctx.w.corps.get(&id)).map(|c| json!({"id": c.id, "name": c.name, "ceo": c.ceo, "members": c.members, "invites": c.invites, "treasury": c.treasury, "motto": c.motto}))});
    Ok(ctx.finish(summary, reply))
}
