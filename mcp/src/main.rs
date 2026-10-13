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
        /// A universe from a newer engine is always refused.
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
        /// A postgres:// URL (read in one read-only snapshot; safe while the server runs) or a
        /// database file (opened as its writer, as `conduct` does: use a copy).
        #[arg(long, default_value = "data/universe.redb")]
        db: String,
    },
    /// The conduct report (operator only): what captains did to and for each other, from the
    /// journal's accepted commands and each captain's record, by captain and by model and harness.
    /// It changes no game state and works on a backup made by another engine version, but a
    /// database file is opened as its writer (it takes the file's lock and may upgrade its schema),
    /// so run it on a copy, never on the original. A postgres:// URL is read in one read-only
    /// snapshot, safe while the server runs.
    Conduct {
        /// A database file (opened as writer: use a copy) or a postgres:// URL (read only).
        #[arg(long, default_value = "data/universe.redb")]
        db: String,
        /// The window: acts in the last this many hours before the newest journal entry.
        #[arg(long, default_value_t = 24)]
        hours: u64,
        /// Print JSON instead of Markdown.
        #[arg(long)]
        json: bool,
    },
    /// Write a consistent copy of a universe to a portable database file (a backup).
    Export {
        /// A database file or a postgres:// URL.
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        from: String,
        #[arg(long)]
        to: PathBuf,
    },
    /// Relaunch (operator only): replace the universe with a new one seeded with identities only.
    /// Kept per captain: call sign, credentials (token, owner token and recovery key hashes, so no
    /// captain reconnects), model, harness, the Autonomous flag, a ban or mute, the latest identity
    /// card and the human's involvement setting; everything else starts fresh in a new galaxy.
    /// A real run (Postgres only) takes the writer lock the way a starting server does (the running
    /// server hands over and answers 503 meanwhile), moves the whole previous universe into the
    /// private schema --archive-schema, writes the new universe in its place, checks it and
    /// releases the lock; if anything fails after the move it moves the previous universe back.
    /// --dry-run reads a snapshot without the lock (production keeps running), prints the audit and
    /// writes nothing; it works on a database file too. --rollback moves the archived universe back
    /// (discarding the current one). Prints a JSON audit: counts, ids and hashes, never content.
    Reseed {
        /// A postgres:// URL (or a database file, for --dry-run only).
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        db: String,
        /// The private schema the previous universe moves to (and comes back from with --rollback).
        #[arg(long, default_value = "pre_relaunch")]
        archive_schema: String,
        /// Seed for the new galaxy (default: derived from the old world's seed and tick).
        #[arg(long)]
        seed: Option<u64>,
        /// Build and audit the new universe from a snapshot read; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// How long to wait for the running server to hand the universe over.
        #[arg(long, default_value_t = 600)]
        wait_secs: u64,
        /// Move the archived universe back into place, dropping the current one.
        #[arg(long)]
        rollback: bool,
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
            // Record the build's commit and start time for /version.
            af_mcp::build_info::init();
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
                        // Never ask a newer server to let go of a universe this build would refuse:
                        // it would release the lock and the game would stop until it restarts.
                        let loc2 = loc.clone();
                        match tokio::task::spawn_blocking(move || af_store::check_takeover(&loc2)).await? {
                            Err(e @ (af_store::StoreError::NewerFile { .. } | af_store::StoreError::NewerEngine { .. })) => {
                                return Err(e.into());
                            }
                            Err(e) => eprintln!("could not read the universe's versions before the handover ({e}); asking anyway"),
                            Ok(()) => {}
                        }
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
            match af_mcp::backup::prepare_dir(&backup_dir)? {
                0 => {}
                n => tracing::info!("removed {n} leftover backup files from {}", backup_dir.display()),
            }
            {
                let dir = backup_dir.clone();
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_secs(60));
                    loop {
                        interval.tick().await;
                        let dir = dir.clone();
                        let _ = tokio::task::spawn_blocking(move || af_mcp::backup::log_resources(&dir)).await;
                    }
                });
            }
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
                            Err(e) if e == af_mcp::game::BACKUP_BUSY => {
                                tracing::info!("automatic backup skipped: another backup is running")
                            }
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
        Cmd::Conduct { db, hours, json } => {
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let store = af_store::Store::open_location(&db, af_store::Access::Snapshot)?;
                let entries = store.raw_entries()?;
                let (world, exact) = store.world_for_report()?;
                let last = entries
                    .iter()
                    .rev()
                    .find_map(|(_, raw)| af_store::from_slice_deep::<serde_json::Value>(raw).ok()?["at_ms"].as_u64())
                    .unwrap_or(0);
                let since = last.saturating_sub(hours * 3_600_000);
                let r = af_mcp::conduct::report(&entries, &world, exact, since);
                if json {
                    println!("{}", serde_json::to_string_pretty(&r)?);
                } else {
                    print!("{}", af_mcp::conduct::markdown(&r));
                }
                Ok(())
            })
            .await??
        }
        Cmd::Export { from, to } => {
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                if to.exists() {
                    anyhow::bail!("{} already exists", to.display());
                }
                let src = af_store::Store::open_location(&from, af_store::Access::Snapshot)?;
                let tmp = to.with_extension("partial");
                let _ = std::fs::remove_file(&tmp);
                let n = src.copy_into(&af_store::Store::create_export(&tmp)?)?;
                std::fs::rename(&tmp, &to)?;
                println!("exported {n} journal entries from {} to {}", src.describe(), to.display());
                verify(&to.display().to_string())
            })
            .await??
        }
        Cmd::Reseed {
            db,
            archive_schema,
            seed,
            dry_run,
            wait_secs,
            rollback,
        } => tokio::task::spawn_blocking(move || reseed(&db, &archive_schema, seed, dry_run, wait_secs, rollback)).await??,
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

/// Take the universe's writer lock the way a starting server does: when another server holds it,
/// check this build may take over (never from a newer engine or schema), ask it to hand over and
/// retry every 2 seconds, for at most `wait_secs`.
fn take_writer_lock(db: &str, wait_secs: u64) -> anyhow::Result<af_store::Store> {
    let deadline = std::time::Instant::now() + Duration::from_secs(wait_secs);
    let mut asked = 0u32;
    loop {
        match af_store::Store::open_location(db, af_store::Access::Writer) {
            Ok(s) => {
                if asked > 0 {
                    eprintln!("the running server handed the universe over");
                }
                return Ok(s);
            }
            Err(af_store::StoreError::Busy(at)) => {
                match af_store::check_takeover(db) {
                    Err(e @ (af_store::StoreError::NewerFile { .. } | af_store::StoreError::NewerEngine { .. })) => return Err(e.into()),
                    Err(e) => eprintln!("could not read the universe's versions before the handover ({e}); asking anyway"),
                    Ok(()) => {}
                }
                if std::time::Instant::now() >= deadline {
                    anyhow::bail!("the server running the universe at {at} did not hand it over within {wait_secs}s; nothing was changed");
                }
                if asked == 0 {
                    eprintln!("another server is running the universe at {at}: asking it to hand over");
                }
                asked += 1;
                if let Err(e) = af_store::pg::request_handoff(db) {
                    eprintln!("handoff request failed: {e}");
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// The reseed audit: counts, ids and hashes of the old and new worlds, never content.
fn reseed_audit(old: &af_core::World, new: &af_core::World, replayed: usize) -> serde_json::Value {
    use af_core::reseed;
    let kept = reseed::kept_ids(old);
    let npcs: Vec<u32> = new.players.values().filter(|p| p.npc.is_some()).map(|p| p.id).collect();
    let (oc, nc) = (reseed::counters(old), reseed::counters(new));
    let counters_down: Vec<&String> = oc
        .iter()
        .filter(|(k, v)| *k != "archive.epoch" && nc.get(*k).is_none_or(|n| n < *v))
        .map(|(k, _)| k)
        .collect();
    serde_json::json!({
        "engine": af_core::ENGINE_VERSION,
        "source": {
            "seq": old.seq, "tick": old.tick, "state_hash": old.state_hash(), "replayed": replayed,
            "season": old.season.number, "epoch": old.archive.epoch,
        },
        "new": {
            "seed": new.seed, "seq": new.seq, "tick": new.tick, "state_hash": new.state_hash(),
            "season": new.season.number, "epoch": new.archive.epoch,
            "founding_era": af_core::founding::founding(new).is_some(),
        },
        "captains_kept": kept.len(),
        "kept_ids": kept,
        "npc_ids": {"count": npcs.len(), "min": npcs.iter().min(), "max": npcs.iter().max()},
        "next_player": new.next_player,
        "counters_never_go_down": counters_down.is_empty(),
        "counters_down": counters_down,
        "counts": {"old": reseed::counts(old), "new": reseed::counts(new)},
    })
}

/// `af-server reseed` (see the subcommand's help).
fn reseed(db: &str, archive: &str, seed: Option<u64>, dry_run: bool, wait_secs: u64, rollback: bool) -> anyhow::Result<()> {
    use af_core::reseed::{ReseedOpts, kept_ids};
    let t0 = std::time::Instant::now();
    let mut phases = serde_json::Map::new();
    let mut lap = |name: &str, since: &mut std::time::Instant| {
        phases.insert(format!("{name}_ms"), serde_json::json!(since.elapsed().as_millis() as u64));
        *since = std::time::Instant::now();
    };
    let mut at = std::time::Instant::now();
    if dry_run && rollback {
        anyhow::bail!("--dry-run and --rollback do not go together");
    }
    if dry_run {
        // A snapshot read: no lock, the running server keeps going.
        let store = af_store::Store::open_location(db, af_store::Access::Snapshot)?;
        let (old, replayed) = store.load_world()?;
        lap("load", &mut at);
        let opts = seed.map(|seed| ReseedOpts { seed }).unwrap_or_else(|| ReseedOpts::for_world(&old));
        let new = af_core::reseed::reseed(&old, &opts);
        lap("build", &mut at);
        let mut audit = reseed_audit(&old, &new, replayed);
        audit["dry_run"] = true.into();
        audit["store"] = store.describe().into();
        audit["phases"] = phases.into();
        println!("{}", serde_json::to_string_pretty(&audit)?);
        return Ok(());
    }
    if !af_store::pg::is_postgres(db) {
        anyhow::bail!("a reseed or rollback runs on Postgres only (a database file supports --dry-run)");
    }
    af_store::pg::valid_archive_schema(archive)?;
    let store = take_writer_lock(db, wait_secs)?;
    lap("lock_wait", &mut at);
    if rollback {
        let moved = store.restore_relocated(archive, true)?;
        let audit = serde_json::json!({"rollback": moved, "store": store.describe(), "total_ms": t0.elapsed().as_millis() as u64});
        println!("{}", serde_json::to_string_pretty(&audit)?);
        drop(store);
        eprintln!("rolled back: the archived universe is in place again; the writer lock is released");
        return Ok(());
    }
    let base = store.base()?;
    if base.engine != af_core::ENGINE_VERSION {
        anyhow::bail!(
            "the universe is on engine {} but this build is {}: let a server of this build adopt it first; nothing was changed",
            base.engine,
            af_core::ENGINE_VERSION
        );
    }
    let (old, replayed) = store.load_world()?;
    eprintln!(
        "loaded the universe at seq {} tick {} (replayed {replayed} journal entries)",
        old.seq, old.tick
    );
    lap("load", &mut at);
    let opts = seed.map(|seed| ReseedOpts { seed }).unwrap_or_else(|| ReseedOpts::for_world(&old));
    let new = af_core::reseed::reseed(&old, &opts);
    let new_hash = new.state_hash();
    let mut audit = reseed_audit(&old, &new, replayed);
    if !audit["counters_never_go_down"].as_bool().unwrap_or(false) {
        anyhow::bail!("an id counter would go down: {}; nothing was changed", audit["counters_down"]);
    }
    lap("build", &mut at);
    let provenance = serde_json::json!({
        "source_engine": base.engine, "source_seq": old.seq, "source_tick": old.tick,
        "source_state_hash": old.state_hash(), "captains_kept": kept_ids(&old).len(),
        "seed": new.seed, "archive_schema": archive, "at_ms": af_mcp::game::now_ms(),
    });
    let genesis = af_store::Genesis {
        seed: new.seed,
        sectors: new.sector_count(),
        rules: new.rules.clone(),
        created_unix_ms: af_mcp::game::now_ms(),
        engine: af_core::ENGINE_VERSION.into(),
    };
    let moved = store.relocate_universe(archive)?;
    eprintln!("moved the previous universe into schema {archive}");
    lap("relocate", &mut at);
    // From here on a failure moves the previous universe back before exiting.
    let checked = (|| -> anyhow::Result<usize> {
        store.init_with_world(&genesis, &new, &provenance)?;
        let (loaded, replayed) = store.load_world()?;
        anyhow::ensure!(replayed == 0, "the new universe replayed {replayed} entries, not 0");
        anyhow::ensure!(
            loaded.state_hash() == new_hash,
            "the stored universe's hash differs from the one built"
        );
        let kept = kept_ids(&old);
        for id in &kept {
            let (o, n) = (&old.players[id], loaded.players.get(id));
            anyhow::ensure!(
                n.is_some_and(|n| n.name == o.name
                    && n.token_hash == o.token_hash
                    && n.owner_token_hash == o.owner_token_hash
                    && n.recovery_hash == o.recovery_hash),
                "captain {id} lost its call sign or credentials"
            );
        }
        Ok(kept.len())
    })();
    lap("init_and_verify", &mut at);
    let kept = match checked {
        Ok(n) => n,
        Err(e) => {
            eprintln!("the new universe failed its checks ({e}): moving the previous universe back");
            match store.restore_relocated(archive, true) {
                Ok(_) => eprintln!("the previous universe is back in place; nothing else changed"),
                Err(r) => eprintln!(
                    "RESTORE FAILED ({r}): the previous universe is still in schema {archive}; run `af-server reseed --rollback --archive-schema {archive}`"
                ),
            }
            return Err(e.context("reseed failed after the move (see above)"));
        }
    };
    audit["dry_run"] = false.into();
    audit["store"] = store.describe().into();
    audit["relocation"] = serde_json::to_value(&moved)?;
    audit["verified"] = serde_json::json!({"replayed": 0, "state_hash_matches": true, "credentials_match": kept});
    phases.insert("total_ms".into(), serde_json::json!(t0.elapsed().as_millis() as u64));
    audit["phases"] = phases.into();
    println!("{}", serde_json::to_string_pretty(&audit)?);
    drop(store);
    eprintln!("done: the new universe is in place and the writer lock is released; the released server exits and restarts on it");
    Ok(())
}

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
