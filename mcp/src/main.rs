//! `af-server`: run the Agentic Frontier universe.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use af_core::Command;
use af_core::cmd::AdminArgs;
use af_core::engine::Actor;
use af_core::model::PlayerKind;
use af_mcp::game::Game;
use af_mcp::gateway::Gateway;
use af_mcp::http::{HttpConfig, router};
use af_mcp::server::GameMcp;
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;

#[derive(Parser)]
#[command(name = "af-server", version, about = "MCP Wars: an MCP-first space trading & conquest universe")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
// Parsed once at startup: the size of the Serve variant does not matter.
#[allow(clippy::large_enum_variant)]
enum Cmd {
    /// Run the multiplayer server: MCP over Streamable HTTP, spectator dashboard, admin API.
    Serve {
        #[arg(long, default_value = "data/universe.redb", env = "AF_DB")]
        db: PathBuf,
        /// Keep the universe in Postgres instead of the --db file (a postgres:// URL).
        /// Several servers may start against it; one holds the writer lock and a
        /// starting server asks it to hand over (how zero-downtime deploys work).
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        database_url: Option<String>,
        #[arg(long, default_value = "127.0.0.1:8787", env = "AF_BIND")]
        bind: SocketAddr,
        /// Public base URL used in connection instructions.
        #[arg(long, env = "AF_PUBLIC_URL")]
        public_url: Option<String>,
        /// Seed for a new universe (ignored when the database already exists).
        #[arg(long, default_value_t = 2002)]
        seed: u64,
        /// Sector count for a new universe.
        #[arg(long, default_value_t = 1_000)]
        sectors: u32,
        /// Seconds per tick for a new universe.
        #[arg(long, default_value_t = 60)]
        tick_secs: u64,
        #[arg(long, env = "AF_ADMIN_TOKEN")]
        admin_token: Option<String>,
        /// Optionally require this key for the public pages (a private universe). Off by default.
        #[arg(long, env = "AF_SPECTATOR_KEY")]
        spectator_key: Option<String>,
        /// Directory for online backups (default: a `backups` folder next to the database).
        #[arg(long, env = "AF_BACKUP_DIR")]
        backup_dir: Option<PathBuf>,
        /// Minutes between automatic backups (0 = off).
        #[arg(long, env = "AF_BACKUP_EVERY_MINS", default_value_t = 60)]
        backup_every_mins: u64,
        /// Automatic backups to keep (oldest are deleted).
        #[arg(long, env = "AF_BACKUP_KEEP", default_value_t = 48)]
        backup_keep: usize,
        /// Header carrying the real client IP behind a trusted proxy (e.g. fly-client-ip). Only set this behind such a proxy.
        #[arg(long, env = "AF_TRUSTED_IP_HEADER")]
        trusted_ip_header: Option<String>,
        /// Self-service registrations allowed per client IP per hour (0 = unlimited).
        #[arg(long, env = "AF_REGISTRATIONS_PER_IP_PER_HOUR", default_value_t = 10)]
        registrations_per_ip_per_hour: u32,
        /// Daily action allowance: pilots regain this many AP per real day, capped at a day and a
        /// half's worth. Applied as a journaled, announced rules change at startup if it differs.
        #[arg(long, env = "AF_AP_PER_DAY")]
        ap_per_day: Option<i64>,
        /// A JSON merge patch for the universe's rules (e.g. '{"war_delay_ticks":240}'), applied as a
        /// journaled rules change at startup when it changes anything. Captains see it announced.
        #[arg(long, env = "AF_RULES_PATCH")]
        rules_patch: Option<String>,
        /// Disable self-service registration (/api/register and join_game).
        #[arg(long)]
        closed: bool,
        /// Extra Host header values to accept (DNS-rebinding protection allows only loopback by default).
        #[arg(long, env = "AF_ALLOW_HOST", value_delimiter = ',')]
        allow_host: Vec<String>,
        /// Rebase a universe recorded by an older engine onto this build's rules
        /// (requires a head snapshot, i.e. the old server was stopped gracefully).
        #[arg(long)]
        adopt_universe: bool,
    },
    /// Single-player local mode over stdio with an embedded universe (for testing with an MCP client).
    Local {
        #[arg(long, default_value = "data/local.redb")]
        db: PathBuf,
        #[arg(long, default_value = "Pilot")]
        name: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value_t = 7)]
        seed: u64,
        #[arg(long, default_value_t = 200)]
        sectors: u32,
        #[arg(long, default_value_t = 20)]
        tick_secs: u64,
        #[arg(long)]
        adopt_universe: bool,
    },
    /// stdio gateway into a running multiplayer server (shared universe).
    Stdio {
        #[arg(long, env = "AF_SERVER", default_value = "http://127.0.0.1:8787")]
        server: String,
        #[arg(long, env = "AF_TOKEN", hide_env_values = true)]
        token: String,
    },
    /// Verify the journal: replay from genesis and compare with snapshot-based restore.
    Verify {
        /// A database file or a postgres:// URL (read only; safe while the server runs).
        #[arg(long, default_value = "data/universe.redb")]
        db: String,
    },
    /// Write a consistent copy of a universe to a portable database file (a backup).
    Export {
        /// A database file or a postgres:// URL.
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        from: String,
        #[arg(long)]
        to: PathBuf,
    },
    /// Copy a universe from a database file into an empty store (a restore or a migration).
    /// Stop the server first: the import takes the writer lock.
    Import {
        #[arg(long)]
        from: PathBuf,
        /// A database file or a postgres:// URL.
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        to: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,rmcp=warn".into()))
        .init();
    match Cli::parse().cmd {
        Cmd::Serve {
            db,
            database_url,
            bind,
            public_url,
            seed,
            sectors,
            tick_secs,
            admin_token,
            spectator_key,
            closed,
            allow_host,
            adopt_universe,
            ap_per_day,
            rules_patch,
            backup_dir,
            backup_every_mins,
            backup_keep,
            trusted_ip_header,
            registrations_per_ip_per_hour,
        } => {
            let postgres = database_url.is_some();
            let loc = database_url.unwrap_or_else(|| db.display().to_string());
            // Bind first. While this server waits for the universe (another server may
            // still be its writer) it answers 503, and /healthz too, so the platform
            // keeps traffic on the old server until this one is ready.
            let std_listener = std::net::TcpListener::bind(bind)?;
            std_listener.set_nonblocking(true)?;
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
            let standby = tokio::spawn(
                axum::serve(tokio::net::TcpListener::from_std(std_listener.try_clone()?)?, standby_router())
                    .with_graceful_shutdown(async {
                        let _ = ready_rx.await;
                    })
                    .into_future(),
            );
            let (world, store, created) = loop {
                let loc2 = loc.clone();
                let opened =
                    tokio::task::spawn_blocking(move || af_mcp::open_world(&loc2, seed, sectors, tick_secs * 1000, adopt_universe)).await?;
                match opened {
                    Ok(v) => break v,
                    Err(e) if matches!(e.downcast_ref::<af_store::StoreError>(), Some(af_store::StoreError::Busy(_))) => {
                        eprintln!("another server is running this universe: asking it to hand over");
                        let loc2 = loc.clone();
                        if let Err(e) = tokio::task::spawn_blocking(move || af_store::pg::request_handoff(&loc2)).await? {
                            eprintln!("handoff request failed: {e}");
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    Err(e) => return Err(e),
                }
            };
            let where_ = store.describe();
            let tick = Duration::from_millis(world.rules.tick_ms);
            let current_rules = serde_json::to_value(&world.rules)?;
            let game = Game::spawn(world, Some(store));
            game.registration
                .per_ip_per_hour
                .store(registrations_per_ip_per_hour, std::sync::atomic::Ordering::Relaxed);
            *game.registration.trusted_ip_header.lock().unwrap() = trusted_ip_header.map(|h| h.to_lowercase());
            let backup_dir = backup_dir.unwrap_or_else(|| {
                if postgres {
                    std::env::temp_dir().join("mcpwars-backups")
                } else {
                    db.parent().map(|p| p.join("backups")).unwrap_or_else(|| PathBuf::from("backups"))
                }
            });
            std::fs::create_dir_all(&backup_dir)?;
            if backup_every_mins > 0 {
                let (g, dir) = (game.clone(), backup_dir.clone());
                tokio::spawn(async move {
                    let mut iv = tokio::time::interval(Duration::from_secs(backup_every_mins * 60));
                    iv.tick().await;
                    loop {
                        iv.tick().await;
                        let dest = dir.join(format!("auto-{}.redb", af_mcp::game::now_ms()));
                        match g.backup(dest.clone()).await {
                            Ok(n) => tracing::info!("backup written: {} ({n} bytes)", dest.display()),
                            Err(e) => tracing::error!("backup failed: {e}"),
                        }
                        // Rotate: keep the newest `backup_keep` automatic backups.
                        if let Ok(rd) = std::fs::read_dir(&dir) {
                            let mut autos: Vec<PathBuf> = rd
                                .flatten()
                                .map(|e| e.path())
                                .filter(|p| {
                                    p.file_name()
                                        .and_then(|n| n.to_str())
                                        .is_some_and(|n| n.starts_with("auto-") && n.ends_with(".redb"))
                                })
                                .collect();
                            autos.sort();
                            while autos.len() > backup_keep {
                                let _ = std::fs::remove_file(autos.remove(0));
                            }
                        }
                    }
                });
            }
            // A galaxy made before engine 1.28 is flat: give it its third dimension once (journaled),
            // before any rules patch can grow it (a new ring already has heights).
            if game
                .read(|w| {
                    let frontier = w.sectors.iter().filter(|s| !s.fedspace).count();
                    frontier > 0 && w.sectors.iter().filter(|s| !s.fedspace && s.z == 0.0).count() * 2 > frontier
                })
                .await
            {
                match game.exec(Actor::System, Command::Admin(AdminArgs::LayoutDepth)).await {
                    Ok(o) => eprintln!("  galaxy              : {}", o.summary),
                    Err(r) => anyhow::bail!("layout_depth refused: {}", r.message),
                }
            }
            // Operator rule settings: applied (and announced to every captain) only when they change something.
            let mut patch = serde_json::Map::new();
            if let Some(n) = ap_per_day.filter(|n| *n > 0) {
                // Core rule: the store holds exactly one check-in interval's AP.
                let checkin = current_rules.get("checkin_minutes").and_then(|v| v.as_i64()).unwrap_or(15);
                let store = (n * checkin / 1_440).max(1);
                patch.insert("ap_per_day".into(), n.into());
                patch.insert("ap_max".into(), store.into());
                patch.insert("ap_start".into(), store.into());
            }
            if let Some(text) = rules_patch.filter(|t| !t.trim().is_empty()) {
                match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(serde_json::Value::Object(m)) => patch.extend(m),
                    _ => anyhow::bail!("AF_RULES_PATCH must be a JSON object"),
                }
            }
            if patch.iter().any(|(k, v)| current_rules.get(k) != Some(v)) {
                let patch = serde_json::Value::Object(patch);
                match game.exec(Actor::System, Command::Admin(AdminArgs::SetRules { patch })).await {
                    Ok(o) => eprintln!("  rules               : {}", o.summary),
                    Err(r) => anyhow::bail!("rules patch refused: {}", r.message),
                }
            }
            game.start_ticker(tick);
            game.start_activity_flush();
            // Summarise the interaction log into the permanent daily table (now, then daily) and prune it.
            af_mcp::archive::start_rollup(&game);
            game.load_fleet().await;
            game.start_fleet();
            game.load_history().await;
            game.start_history();
            let public_url = public_url.unwrap_or_else(|| format!("http://{bind}"));
            let mut hosts = allow_host;
            if let Ok(u) = reqwest::Url::parse(&public_url)
                && let Some(h) = u.host_str()
            {
                hosts.push(h.to_string());
            }
            let locked_map = spectator_key.is_some();
            let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let app = router(
                game.clone(),
                HttpConfig {
                    public_url: public_url.clone(),
                    admin_token,
                    spectator_key,
                    open_registration: !closed,
                    allowed_hosts: hosts,
                    backup_dir: Some(backup_dir.clone()),
                },
            )
            .layer(axum::middleware::from_fn({
                let released = released.clone();
                move |req: axum::extract::Request, next: axum::middleware::Next| {
                    let released = released.clone();
                    async move {
                        // A released server still holds the galaxy in memory but can no
                        // longer save: keep serving pages and reads, refuse anything that acts.
                        let reading =
                            matches!(*req.method(), http::Method::GET | http::Method::HEAD) && !req.uri().path().starts_with("/mcp");
                        if released.load(std::sync::atomic::Ordering::Relaxed) && !reading {
                            return unavailable(req.headers(), "MCP Wars is moving to a new version. Retry in a few seconds.");
                        }
                        next.run(req).await
                    }
                }
            }));
            let _ = ready_tx.send(());
            standby.await??;
            let listener = tokio::net::TcpListener::from_std(std_listener)?;
            eprintln!("MCP Wars {} universe at {where_}", if created { "created new" } else { "resumed" });
            eprintln!("  home page           : {public_url}/");
            if locked_map {
                eprintln!("  live map            : {public_url}/watch?key=<spectator key>");
            } else {
                eprintln!("  live map            : {public_url}/watch");
            }
            eprintln!("  agent entry points  : {public_url}/agents.txt   and   {public_url}/llms.txt");
            eprintln!("  MCP endpoint        : {public_url}/mcp  (Authorization: Bearer <token>)  or  {public_url}/mcp/<token>");
            eprintln!(
                "  register            : curl -X POST {public_url}/api/register -H 'content-type: application/json' -d '{{\"name\":\"MyBot\",\"model\":\"my-model\"}}'"
            );
            // Postgres: listen for a successor asking to take over.
            let (handoff_tx, handoff_rx) = tokio::sync::oneshot::channel::<()>();
            if postgres {
                let loc2 = loc.clone();
                std::thread::spawn(move || {
                    loop {
                        match af_store::pg::wait_for_handoff(&loc2) {
                            Ok(true) => {
                                let _ = handoff_tx.send(());
                                return;
                            }
                            Ok(false) => tracing::warn!("handoff listener disconnected; reconnecting"),
                            Err(e) => tracing::warn!("handoff listener: {e}; retrying"),
                        }
                        std::thread::sleep(Duration::from_secs(5));
                    }
                });
            }
            // Don't wait for open spectator streams (they never close on their own): on
            // Ctrl-C or SIGTERM (what hosting platforms send on deploy) write a head
            // snapshot and exit, so the next start (with --adopt-universe) resumes cleanly.
            let signal = async {
                #[cfg(unix)]
                {
                    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = term.recv() => {}
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = tokio::signal::ctrl_c().await;
                }
            };
            let shutdown = async {
                tokio::select! {
                    _ = signal => {
                        eprintln!("shutting down: writing snapshot");
                        game.flush_activity().await;
                        game.save_fleet().await;
                        game.save_history().await;
                        game.snapshot().await;
                    }
                    Ok(()) = handoff_rx => {
                        // Hand over, then stay up (answering 503) until the platform
                        // stops this instance: exiting early could get it restarted.
                        eprintln!("a new server asked to take over: writing snapshot and releasing the universe");
                        released.store(true, std::sync::atomic::Ordering::Relaxed);
                        game.save_fleet().await;
                        game.save_history().await;
                        game.release().await;
                        eprintln!("released; waiting to be stopped");
                        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
                        tokio::select! {
                            _ = tokio::signal::ctrl_c() => {}
                            _ = term.recv() => {}
                            _ = successor_missing(loc.clone()) => {
                                // The successor never took over (a failed deploy): exit so the
                                // platform restarts this server, which then takes the universe back.
                                eprintln!("no server took over within 60s of the handover: exiting to be restarted");
                                std::process::exit(3);
                            }
                        }
                    }
                }
            };
            tokio::select! {
                r = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()) => r?,
                _ = shutdown => {}
            }
        }
        Cmd::Local {
            db,
            name,
            model,
            seed,
            sectors,
            tick_secs,
            adopt_universe,
        } => {
            let loc = db.display().to_string();
            let (world, store, _) =
                tokio::task::spawn_blocking(move || af_mcp::open_world(&loc, seed, sectors, tick_secs * 1000, adopt_universe)).await??;
            let existing = world.player_by_name(&name).map(|p| p.id);
            let tick = Duration::from_millis(world.rules.tick_ms);
            let game = Game::spawn(world, Some(store));
            game.start_ticker(tick);
            game.start_activity_flush();
            // Summarise the interaction log into the permanent daily table (now, then daily) and prune it.
            af_mcp::archive::start_rollup(&game);
            let pid = match existing {
                Some(p) => p,
                None => {
                    game.register(&name, model, PlayerKind::Human)
                        .await
                        .map_err(|r| anyhow::anyhow!("{r}"))?
                        .0
                }
            };
            eprintln!("MCP Wars local stdio mode: playing as {name} (player {pid}) in an embedded single-process universe.");
            let svc = GameMcp::new(game.clone(), Some(pid), "stdio://local".into())
                .serve(rmcp::transport::stdio())
                .await?;
            svc.waiting().await?;
            game.snapshot().await;
        }
        Cmd::Stdio { server, token } => {
            let gw = Gateway::connect(&server, &token).await?;
            let svc = gw.serve(rmcp::transport::stdio()).await?;
            svc.waiting().await?;
        }
        Cmd::Verify { db } => tokio::task::spawn_blocking(move || verify(&db)).await??,
        Cmd::Export { from, to } => {
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                if to.exists() {
                    anyhow::bail!("{} already exists", to.display());
                }
                let src = af_store::Store::open_location(&from, af_store::Access::Snapshot)?;
                let tmp = to.with_extension("partial");
                let _ = std::fs::remove_file(&tmp);
                let n = src.copy_into(&af_store::Store::open(&tmp)?)?;
                std::fs::rename(&tmp, &to)?;
                println!("exported {n} journal entries from {} to {}", src.describe(), to.display());
                verify(&to.display().to_string())
            })
            .await??
        }
        Cmd::Import { from, to } => {
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let src = af_store::Store::open(&from)?;
                src.check_engine()?;
                let dst = af_store::Store::open_location(&to, af_store::Access::Writer)?;
                let n = src.copy_into(&dst)?;
                println!("imported {n} journal entries into {}", dst.describe());
                drop(dst);
                verify(&to)
            })
            .await??
        }
    }
    Ok(())
}

/// Resolves once no server has held the universe's writer lock for 60 seconds.
async fn successor_missing(loc: String) {
    let mut free_since: Option<std::time::Instant> = None;
    loop {
        tokio::time::sleep(Duration::from_secs(10)).await;
        let l = loc.clone();
        let present = tokio::task::spawn_blocking(move || af_store::pg::writer_present(&l))
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(true);
        if present {
            free_since = None;
        } else if free_since.get_or_insert_with(std::time::Instant::now).elapsed() >= Duration::from_secs(50) {
            return;
        }
    }
}

/// The router a starting server answers with until it holds the universe.
fn standby_router() -> axum::Router {
    axum::Router::new().fallback(|h: http::HeaderMap| async move { unavailable(&h, "MCP Wars is starting up. Retry in a few seconds.") })
}

/// 503 with Retry-After: a page that reloads itself for browsers, JSON for everything else.
fn unavailable(h: &http::HeaderMap, msg: &'static str) -> axum::response::Response {
    use axum::response::IntoResponse;
    let html = h
        .get(http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/html"));
    let retry = [(http::header::RETRY_AFTER, "5"), (http::header::CACHE_CONTROL, "no-store")];
    if html {
        (http::StatusCode::SERVICE_UNAVAILABLE, retry, axum::response::Html(MOVING_HTML)).into_response()
    } else {
        let body = serde_json::json!({"error": {"code": "server_moving", "message": msg}, "retry_after_secs": 5});
        (http::StatusCode::SERVICE_UNAVAILABLE, retry, axum::Json(body)).into_response()
    }
}

/// Shown to browsers for the few seconds a new version takes over.
const MOVING_HTML: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="refresh" content="4"><title>MCP Wars</title>
<style>
:root { color-scheme: dark; }
body { margin: 0; min-height: 100vh; display: grid; place-items: center; background: radial-gradient(ellipse at 50% 40%, #12162e, #04050c 70%); color: #eceefe; font: 17px/1.5 system-ui, sans-serif; }
main { padding: 24px; text-align: center; max-width: 34rem; }
.mark { font: 700 0.95rem system-ui, sans-serif; letter-spacing: 0.06em; color: #9097bd; margin: 0 0 28px; }
h1 { font-weight: 600; font-size: clamp(1.5rem, 5vw, 2.2rem); line-height: 1.15; margin: 0 0 14px; }
p { color: #9097bd; margin: 0; }
.dot { display: inline-block; width: 9px; height: 9px; border-radius: 50%; background: #f0c070; margin-right: 10px; vertical-align: middle; animation: beat 1.4s infinite; }
@keyframes beat { 0%, 100% { opacity: 1; } 50% { opacity: 0.25; } }
@media (prefers-reduced-motion: reduce) { .dot { animation: none; } }
</style></head>
<body><main>
<p class="mark">MCP Wars</p>
<h1><span class="dot" aria-hidden="true"></span>Jumping to a new version</h1>
<p>The galaxy is back in a few seconds. This page reloads by itself, and nothing in the universe is lost.</p>
</main></body></html>"#;

/// Replay from the base and compare with the snapshot-based restore.
fn verify(db: &str) -> anyhow::Result<()> {
    // A snapshot read: consistent even while the server keeps writing.
    let store = af_store::Store::open_location(db, af_store::Access::Snapshot)?;
    let (restored, replayed) = store.load_world()?;
    let full = store.replay_from_base()?;
    let norm = |w: &af_core::World| {
        let mut c = w.clone();
        c.now_ms = 0;
        c.state_hash()
    };
    let (a, b) = (norm(&restored), norm(&full));
    println!("journal entries: {}  (replayed after snapshot: {replayed})", store.journal_len()?);
    println!("tick {}  seq {}", restored.tick, restored.seq);
    println!("snapshot+replay hash: {a}\nbase replay hash    : {b}");
    if a == b {
        println!("OK: deterministic replay verified");
    } else {
        println!("MISMATCH");
        std::process::exit(1);
    }
    Ok(())
}
