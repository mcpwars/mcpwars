//! `af-harness`: simulations, scripted bots, the LLM agent orchestrator and a
//! tiny human REPL client.

mod orchestrator;

use af_harness::{bots, client};

use std::io::{BufRead, Write};
use std::path::PathBuf;

use af_core::Command;
use af_core::engine::Actor;
use af_core::galaxy::{GenesisConfig, generate};
use af_core::model::PlayerKind;
use af_mcp::game::Game;
use bots::{Bot, Style, ToolCaller};
use clap::{Parser, Subcommand};
use client::{InProc, Remote};
use serde_json::json;

#[derive(Parser)]
#[command(name = "af-harness", version, about = "Agentic Frontier harness")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// In-process balance simulation with scripted bots (no network, no API keys).
    Sim {
        #[arg(long, default_value_t = 2002)]
        seed: u64,
        #[arg(long, default_value_t = 1_000)]
        sectors: u32,
        #[arg(long, default_value_t = 12)]
        bots: usize,
        #[arg(long, default_value_t = 1440)]
        ticks: u64,
        /// Each bot plays a turn every N ticks.
        #[arg(long, default_value_t = 10)]
        turn_every: u64,
        #[arg(long, default_value_t = 240)]
        report_every: u64,
        /// Tool calls a bot may make per turn.
        #[arg(long, default_value_t = 10)]
        calls: u32,
        /// JSON merge patch applied to the rules before play (e.g. '{"ap_per_day":2880,"ap_max":40}').
        #[arg(long)]
        rules_patch: Option<String>,
        /// Print one summary line at the end instead of periodic reports.
        #[arg(long)]
        summary: bool,
        /// Bot styles, cycled over the bots (merchant, corsair, builder, expert, hauler).
        #[arg(long, default_value = "merchant,merchant,corsair,builder")]
        styles: String,
        /// Careers, cycled over the bots ("" = the style's own career, "none" = no career).
        #[arg(long, default_value = "")]
        careers: String,
        /// Late joiners: N@TICK registers N more bots (same style cycle) at that tick.
        #[arg(long)]
        late: Option<String>,
        /// Print one line per simulated day (leader, spread) and count lead changes.
        #[arg(long)]
        daily: bool,
        /// Write per-bot results as JSON to this file.
        #[arg(long)]
        json: Option<PathBuf>,
        /// Per-bot check-in cadences in ticks, cycled over the bots (overrides --turn-every).
        #[arg(long)]
        cadences: Option<String>,
    },
    /// Run scripted bots against a live server over MCP (populates a universe).
    Bots {
        #[arg(long, default_value = "http://127.0.0.1:8787")]
        server: String,
        #[arg(long, default_value_t = 4)]
        count: usize,
        /// Comma-separated styles cycled over the bots: merchant, corsair, builder.
        #[arg(long, default_value = "merchant,corsair,builder,merchant")]
        styles: String,
        #[arg(long, default_value_t = 30)]
        interval_secs: u64,
        #[arg(long, default_value = "Bot")]
        prefix: String,
        /// Stop after N rounds (0 = forever).
        #[arg(long, default_value_t = 0)]
        rounds: u64,
        /// Where bot tokens are remembered between runs.
        #[arg(long, default_value = "data/bots.json")]
        state_file: PathBuf,
    },
    /// Run the LLM agent orchestrator from a TOML config.
    Agents {
        #[arg(long, default_value = "agents.toml")]
        config: PathBuf,
    },
    /// Controlled evaluation: run scenarios x conditions x candidates x seeds in process (see docs/EVAL.md).
    Eval {
        /// Scenario files (eval/scenarios/*.toml).
        #[arg(long = "scenario", required = true)]
        scenarios: Vec<PathBuf>,
        /// Candidates: scripted:merchant@1, scripted:idle@1, scripted:gullible-merchant@1, openai:MODEL, anthropic:MODEL.
        #[arg(long = "candidate", required = true)]
        candidates: Vec<String>,
        /// Seed split from the scenario file: dev or heldout.
        #[arg(long, default_value = "dev")]
        split: String,
        /// Override the scenario's seeds (comma-separated).
        #[arg(long, value_delimiter = ',')]
        seeds: Option<Vec<u64>>,
        /// Candidate id to compare against per seed (e.g. scripted:merchant@1).
        #[arg(long)]
        baseline: Option<String>,
        #[arg(long, default_value = "eval/runs/latest")]
        out: PathBuf,
        /// Repetitions per seed (each gets its own episode id and trace).
        #[arg(long, default_value_t = 1)]
        repeats: u32,
        /// Required for API candidates: they make paid model calls.
        #[arg(long)]
        allow_paid: bool,
        /// Allow paid runs from a tree with uncommitted changes (the diff is archived).
        #[arg(long)]
        allow_dirty: bool,
    },
    /// Interactive REPL for humans: `tool_name {json args}`.
    Play {
        #[arg(long, default_value = "http://127.0.0.1:8787")]
        server: String,
        #[arg(long, env = "AF_TOKEN")]
        token: Option<String>,
        /// Register a new pilot with this name if no token is given.
        #[arg(long)]
        name: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .init();
    match Cli::parse().cmd {
        Cmd::Sim {
            seed,
            sectors,
            bots,
            ticks,
            turn_every,
            report_every,
            calls,
            rules_patch,
            summary,
            styles,
            careers,
            late,
            daily,
            json,
            cadences,
        } => {
            let opts = SimOpts {
                styles,
                careers,
                late,
                daily,
                json,
                cadences,
            };
            sim(
                seed,
                sectors,
                bots,
                ticks,
                turn_every,
                report_every,
                calls,
                rules_patch,
                summary,
                opts,
            )
            .await
        }
        Cmd::Bots {
            server,
            count,
            styles,
            interval_secs,
            prefix,
            rounds,
            state_file,
        } => live_bots(server, count, styles, interval_secs, prefix, rounds, state_file).await,
        Cmd::Agents { config } => {
            let text = std::fs::read_to_string(&config)?;
            let cfg: orchestrator::OrchestratorCfg = toml::from_str(&text)?;
            orchestrator::run(cfg).await
        }
        Cmd::Play { server, token, name } => play(server, token, name).await,
        Cmd::Eval {
            scenarios,
            candidates,
            split,
            seeds,
            baseline,
            out,
            repeats,
            allow_paid,
            allow_dirty,
        } => {
            let args = af_harness::eval::runner::SuiteArgs {
                scenarios,
                candidates,
                split,
                seeds,
                repeats,
                out: out.clone(),
                baseline,
                allow_paid,
                allow_dirty,
            };
            af_harness::eval::runner::run_suite(&args).await?;
            println!("{}", std::fs::read_to_string(out.join("results.md"))?);
            Ok(())
        }
    }
}

struct SimOpts {
    styles: String,
    careers: String,
    late: Option<String>,
    daily: bool,
    json: Option<PathBuf>,
    cadences: Option<String>,
}

/// One simulated captain and what the harness measured about it.
struct SimBot {
    c: InProc,
    bot: Bot,
    style: Style,
    joined: u64,
    every: u64,
    checkins: u64,
    ap_spent: i64,
    /// Earned (net worth minus endowment) when the late joiners arrived.
    earned_at_late: Option<i64>,
}

#[allow(clippy::too_many_arguments)]
async fn sim(
    seed: u64,
    sectors: u32,
    n: usize,
    ticks: u64,
    turn_every: u64,
    report_every: u64,
    calls: u32,
    rules_patch: Option<String>,
    summary: bool,
    opts: SimOpts,
) -> anyhow::Result<()> {
    // The patch also shapes the galaxy (port trade strength and sizes are rolled at creation),
    // then is applied as a rules change like AF_RULES_PATCH on a live server.
    let mut cfg = GenesisConfig::new(seed, sectors);
    if let Some(p) = &rules_patch {
        fn merge(t: &mut serde_json::Value, p: &serde_json::Value) {
            match (t, p) {
                (serde_json::Value::Object(t), serde_json::Value::Object(p)) => {
                    for (k, v) in p {
                        merge(t.entry(k.clone()).or_insert(serde_json::Value::Null), v);
                    }
                }
                (t, p) => *t = p.clone(),
            }
        }
        let mut v = serde_json::to_value(&cfg.rules)?;
        merge(&mut v, &serde_json::from_str(p)?);
        cfg.rules = serde_json::from_value(v)?;
    }
    let world = generate(&cfg);
    let game = Game::spawn(world, None);
    if let Some(p) = rules_patch {
        let patch: serde_json::Value = serde_json::from_str(&p)?;
        game.exec(Actor::System, Command::Admin(af_core::cmd::AdminArgs::SetRules { patch }))
            .await
            .map_err(|r| anyhow::anyhow!("{r}"))?;
    }
    let styles: Vec<Style> = opts.styles.split(',').map(|s| Style::parse(s.trim())).collect();
    let careers: Vec<String> = opts.careers.split(',').map(|s| s.trim().to_string()).collect();
    let (late_n, late_tick) = match &opts.late {
        Some(l) => {
            let (a, b) = l.split_once('@').ok_or_else(|| anyhow::anyhow!("--late is N@TICK"))?;
            (a.parse::<usize>()?, b.parse::<u64>()?)
        }
        None => (0, u64::MAX),
    };
    let cadences: Vec<u64> = match &opts.cadences {
        Some(c) => c.split(',').map(|x| x.trim().parse::<u64>()).collect::<Result<_, _>>()?,
        None => vec![turn_every],
    };
    #[allow(clippy::too_many_arguments)]
    async fn add(
        game: &Game,
        agents: &mut Vec<SimBot>,
        i: usize,
        styles: &[Style],
        careers: &[String],
        cadences: &[u64],
        seed: u64,
        tick: u64,
    ) -> anyhow::Result<()> {
        let style = styles[i % styles.len()];
        let name = format!("{:?}{}", style, i + 1);
        let (pid, _) = game
            .register(&name, Some(format!("scripted:{style:?}").to_lowercase()), PlayerKind::Agent)
            .await
            .map_err(|r| anyhow::anyhow!("{r}"))?;
        let mut bot = Bot::new(style, i as u64 + seed);
        bot.career = match careers[i % careers.len()].as_str() {
            "" => Some(style.career().to_string()),
            "none" => None,
            c => Some(c.to_string()),
        };
        agents.push(SimBot {
            c: InProc { game: game.clone(), pid },
            bot,
            style,
            joined: tick,
            every: cadences[i % cadences.len()],
            checkins: 0,
            ap_spent: 0,
            earned_at_late: None,
        });
        Ok(())
    }
    let mut agents: Vec<SimBot> = vec![];
    for i in 0..n {
        add(&game, &mut agents, i, &styles, &careers, &cadences, seed, 0).await?;
    }
    let earned_of = |game: &Game, ids: Vec<u32>| {
        let game = game.clone();
        async move {
            game.read(move |w| {
                ids.iter()
                    .map(|id| af_core::player::net_worth(w, *id) - af_core::player::endowment(w, &w.players[id]))
                    .collect::<Vec<i64>>()
            })
            .await
        }
    };
    let started = std::time::Instant::now();
    let mut leaders: Vec<String> = vec![];
    for t in 1..=ticks {
        game.exec(Actor::System, Command::Tick).await.map_err(|r| anyhow::anyhow!("{r}"))?;
        if t == late_tick {
            let now = earned_of(&game, agents.iter().map(|a| a.c.pid).collect()).await;
            for (a, e) in agents.iter_mut().zip(now) {
                a.earned_at_late = Some(e);
            }
            for k in 0..late_n {
                add(&game, &mut agents, n + k, &styles, &careers, &cadences, seed, t).await?;
            }
        }
        for (i, a) in agents.iter_mut().enumerate() {
            if (t + i as u64).is_multiple_of(a.every) {
                let pid = a.c.pid;
                let before = game.read(move |w| w.players[&pid].ap).await;
                a.bot.turn(&a.c, calls as usize).await;
                let after = game.read(move |w| w.players[&pid].ap).await;
                a.ap_spent += (before - after).max(0);
                a.checkins += 1;
            }
        }
        if opts.daily && t.is_multiple_of(1_440) {
            let ids: Vec<u32> = agents.iter().map(|a| a.c.pid).collect();
            let earned = earned_of(&game, ids.clone()).await;
            let mut rows: Vec<(i64, String)> = earned
                .iter()
                .zip(&agents)
                .map(|(e, a)| (*e, format!("{:?}{}/{}m", a.style, a.c.pid, a.every)))
                .collect();
            rows.sort_by_key(|r| std::cmp::Reverse(r.0));
            let total: i64 = rows.iter().map(|r| r.0.max(0)).sum::<i64>().max(1);
            let median = rows[rows.len() / 2].0;
            println!(
                "day {:>2}: leader {} {} | 2nd {} {} | median {} | leader share {}% | lead over 2nd {}%",
                t / 1_440,
                rows[0].1,
                rows[0].0,
                rows[1].1,
                rows[1].0,
                median,
                rows[0].0.max(0) * 100 / total,
                (rows[0].0 - rows[1].0) * 100 / rows[1].0.max(1)
            );
            leaders.push(rows[0].1.clone());
        }
        if !summary && !opts.daily && (t % report_every == 0 || t == ticks) {
            report(&game, t, &agents).await;
        }
    }
    if opts.daily {
        let changes = leaders.windows(2).filter(|w| w[0] != w[1]).count();
        let last = leaders.last().cloned().unwrap_or_default();
        let since = leaders.iter().rposition(|l| *l != last).map(|i| i + 2).unwrap_or(1);
        println!("lead changes {changes}; final leader {last} has led since day {since}");
    }
    // Per-bot results.
    let rules = game.read(|w| w.rules.clone()).await;
    let regen_between = |from: u64, to: u64| -> i64 { (from + 1..=to).map(|t| af_core::player::ap_regen_this_tick(&rules, t)).sum() };
    let ids: Vec<u32> = agents.iter().map(|a| a.c.pid).collect();
    let rows = game
        .read(move |w| {
            ids.iter()
                .map(|id| {
                    let p = &w.players[id];
                    let s = af_core::careers::scores_json(w, p);
                    (
                        af_core::player::net_worth(w, *id) - af_core::player::endowment(w, p),
                        p.ap,
                        p.stats.trades,
                        p.stats.kills,
                        format!("{:?}", p.ship.class),
                        p.ship.holds,
                        p.profession.map(|c| format!("{c:?}").to_lowercase()),
                        s,
                        w.planets.values().filter(|pl| pl.owner == Some(*id)).count(),
                        (
                            p.stats.deaths,
                            p.stats.fighters_lost,
                            p.stats.trade_profit,
                            p.experience,
                            (
                                p.stats.tolls_collected,
                                p.stats.salvage,
                                p.stats.fed_rewards,
                                p.stats.concession_income,
                                p.stats.tolls_paid,
                            ),
                        ),
                    )
                })
                .collect::<Vec<_>>()
        })
        .await;
    let mut out = vec![];
    for (a, row) in agents.iter().zip(&rows) {
        let available = rules.ap_start + regen_between(a.joined, ticks);
        out.push(json!({
            "name": format!("{:?}{}", a.style, a.c.pid), "style": format!("{:?}", a.style).to_lowercase(), "career": row.6,
            "joined": a.joined, "every": a.every, "earned": row.0, "earned_at_late": a.earned_at_late,
            "ap_spent": a.ap_spent, "ap_available": available, "ap_util_pct": a.ap_spent * 100 / available.max(1),
            "cr_per_ap": row.0 / a.ap_spent.max(1), "checkins": a.checkins, "calls": a.bot.stats.calls, "refusals": a.bot.stats.refusals,
            "calls_per_checkin": a.bot.stats.calls / a.checkins.max(1), "trades": row.2, "kills": row.3, "ship": row.4, "holds": row.5,
            "scores": row.7, "planets": row.8,
            "deaths": row.9.0, "fighters_lost": row.9.1, "trade_profit": row.9.2, "experience": row.9.3, "tolls_collected": row.9.4.0, "salvage": row.9.4.1, "fed_rewards": row.9.4.2, "concession_income": row.9.4.3, "tolls_paid": row.9.4.4,
        }));
    }
    if let Some(path) = &opts.json {
        std::fs::write(path, serde_json::to_string_pretty(&out)?)?;
    }
    if summary || opts.daily {
        let mut earned: Vec<i64> = rows.iter().map(|r| r.0).collect();
        earned.sort();
        let total: i64 = earned.iter().map(|e| (*e).max(0)).sum::<i64>().max(1);
        let trades: i64 = rows.iter().map(|r| r.2).sum();
        let util: i64 = out.iter().map(|o| o["ap_util_pct"].as_i64().unwrap_or(0)).sum::<i64>() / out.len().max(1) as i64;
        let mut by_style: std::collections::BTreeMap<String, Vec<i64>> = Default::default();
        for o in &out {
            by_style
                .entry(o["style"].as_str().unwrap_or("").to_string())
                .or_default()
                .push(o["earned"].as_i64().unwrap_or(0));
        }
        let styles_txt: Vec<String> = by_style
            .iter_mut()
            .map(|(k, v)| {
                v.sort();
                format!("{k} {}", v[v.len() / 2])
            })
            .collect();
        println!(
            "earned median {} max {} min {} | top share {}% | trades {} | AP used {}% | by style: {}",
            earned[earned.len() / 2],
            earned.last().unwrap_or(&0),
            earned.first().unwrap_or(&0),
            earned.last().unwrap_or(&0).max(&0) * 100 / total,
            trades,
            util,
            styles_txt.join(", ")
        );
        return Ok(());
    }
    let calls: u64 = agents.iter().map(|a| a.bot.stats.calls).sum();
    let refusals: u64 = agents.iter().map(|a| a.bot.stats.refusals).sum();
    println!(
        "\nsimulated {ticks} ticks with {} bots in {:.1}s: {calls} tool calls, {refusals} refusals ({:.1}%)",
        agents.len(),
        started.elapsed().as_secs_f64(),
        refusals as f64 * 100.0 / calls.max(1) as f64
    );
    Ok(())
}

async fn report(game: &Game, t: u64, agents: &[SimBot]) {
    let ids: Vec<u32> = agents.iter().map(|a| a.c.pid).collect();
    let text = game.read(move |w| {
        let mut out = format!("\n=== tick {t} ===\n");
        let mut rows: Vec<_> = ids.iter().map(|id| {
            let p = &w.players[id];
            (af_core::player::net_worth(w, *id), p)
        }).collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.0));
        for (nw, p) in rows {
            out.push_str(&format!("  {:<14} nw {:>9}  cr {:>8} bank {:>8}  {:<18} ftr {:>5} sec {:>4} trades {:>4} profit {:>8} kills {} deaths {} planets {}\n",
                p.name, nw, p.credits, p.bank, format!("{:?}", p.ship.class), p.ship.fighters, p.sector, p.stats.trades, p.stats.trade_profit,
                p.stats.kills, p.stats.deaths, w.planets.values().filter(|pl| pl.owner == Some(p.id)).count()));
        }
        let cit: Vec<u8> = w.planets.values().filter(|p| p.owner.is_some()).map(|p| p.citadel).collect();
        let deployed: i64 = w.sectors.iter().filter_map(|s| s.fighters.as_ref()).map(|f| f.count).sum();
        let skarn_kills: i64 = w.players.values().filter(|p| p.npc.is_some()).map(|p| p.stats.kills).sum();
        out.push_str(&format!("  owned planets {} citadels {:?} | deployed fighters {} | corps {} | npc kills {} | market avg {:?}\n",
            cit.len(), cit, deployed, w.corps.len(), skarn_kills, w.market_history.last()));
        for n in w.news.iter().rev().take(4) {
            out.push_str(&format!("  news t{}: {}\n", n.tick, n.headline));
        }
        out
    }).await;
    print!("{text}");
    let _ = std::io::stdout().flush();
}

async fn live_bots(
    server: String,
    count: usize,
    styles: String,
    interval: u64,
    prefix: String,
    rounds: u64,
    state_file: PathBuf,
) -> anyhow::Result<()> {
    let styles: Vec<Style> = styles.split(',').map(|s| Style::parse(s.trim())).collect();
    let mut tokens = orchestrator::load_tokens(&state_file);
    let mut handles = vec![];
    for i in 0..count {
        let style = styles[i % styles.len()];
        let name = format!("{prefix}-{:?}-{}", style, i + 1);
        let server = server.clone();
        let token = match tokens.get(&name) {
            Some(t) => t.clone(),
            None => {
                let t = client::register(&server, &name, &format!("scripted:{style:?}").to_lowercase()).await?;
                tokens.insert(name.clone(), t.clone());
                orchestrator::save_tokens(&state_file, &tokens);
                t
            }
        };
        handles.push(tokio::spawn(async move {
            let mut remote = match Remote::connect(&server, &token).await {
                Ok(r) => r,
                Err(e) => return eprintln!("[{name}] connect failed: {e}"),
            };
            let mut bot = Bot::new(style, i as u64 * 31 + 7);
            let mut round = 0;
            loop {
                round += 1;
                bot.turn(&remote, 12).await;
                let (_, st) = remote.call("get_status", json!({})).await;
                if st.is_null() {
                    // Server restarted: reconnect.
                    if let Ok(r) = Remote::connect(&server, &token).await {
                        remote = r;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
                eprintln!(
                    "[{name}] round {round}: sector {} credits {} nw {} ({} calls, {} refusals)",
                    st["result"]["sector"], st["result"]["credits"], st["result"]["net_worth"], bot.stats.calls, bot.stats.refusals
                );
                if rounds > 0 && round >= rounds {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    Ok(())
}

async fn play(server: String, token: Option<String>, name: Option<String>) -> anyhow::Result<()> {
    let token = match (token, name) {
        (Some(t), _) => t,
        (None, Some(n)) => {
            let t = client::register(&server, &n, "human").await?;
            println!("Registered {n}. Your token (keep it!): {t}");
            t
        }
        (None, None) => anyhow::bail!("pass --token or --name"),
    };
    let remote = Remote::connect(&server, &token).await?;
    let tools = remote.tools().await?;
    println!(
        "Connected. {} tools. Type `help`, `tools`, or `<tool> {{json}}` (e.g. `warp {{\"destination_sector\": 2}}`). Ctrl-D to quit.",
        tools.len()
    );
    let (_, _, b) = remote.call_full("get_briefing", json!({})).await;
    println!("{b}");
    let stdin = std::io::stdin();
    loop {
        print!("af> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "help" || line == "tools" {
            for t in &tools {
                println!(
                    "  {:<18} {}",
                    t.name,
                    t.description.as_deref().unwrap_or("").chars().take(110).collect::<String>()
                );
            }
            continue;
        }
        let (name, rest) = line.split_once(' ').unwrap_or((line, "{}"));
        let args: serde_json::Value = match serde_json::from_str(rest) {
            Ok(v) => v,
            Err(e) => {
                println!("bad JSON: {e}");
                continue;
            }
        };
        let (_, _, text) = remote.call_full(name, args).await;
        println!("{text}");
    }
    Ok(())
}
