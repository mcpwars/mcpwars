//! HTTP surface: MCP (Streamable HTTP), registration, spectator feed, admin.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use af_core::cmd::AdminArgs;
use af_core::engine::Actor;
use af_core::model::PlayerKind;
use af_core::{Command, query, spectator};
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::{self, Stream, StreamExt};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_stream::wrappers::BroadcastStream;

use crate::activity::Call;
use crate::game::{FeedItem, Game};
use crate::server::GameMcp;

const WATCH_HTML: &str = include_str!("../../web/watch.html");
const WATCH_JS: &str = include_str!("../../web/watch.js");
const WATCH_CSS: &str = include_str!("../../web/watch.css");
const LANDING_HTML: &str = include_str!("../../web/landing.html");
const LANDING_JS: &str = include_str!("../../web/landing.js");
const LANDING_CSS: &str = include_str!("../../web/landing.css");
const SCOREBOARD_HTML: &str = include_str!("../../web/scoreboard.html");
const SCOREBOARD_JS: &str = include_str!("../../web/scoreboard.js");
const CONNECT_HTML: &str = include_str!("../../web/connect.html");
const CONNECT_JS: &str = include_str!("../../web/connect.js");

const SYSOP_HTML: &str = include_str!("../../web/sysop.html");
const SYSOP_JS: &str = include_str!("../../web/sysop.js");
const SYSOP_MAP_JS: &str = include_str!("../../web/sysop-map.js");
const GALAXY3D_JS: &str = include_str!("../../web/galaxy3d.js");
const SYSOP_CSS: &str = include_str!("../../web/sysop.css");
const FAVICON_SVG: &str = include_str!("../../web/favicon.svg");
const FAVICON_32_PNG: &[u8] = include_bytes!("../../web/favicon-32.png");
const APPLE_TOUCH_ICON_PNG: &[u8] = include_bytes!("../../web/apple-touch-icon.png");
const OG_PNG: &[u8] = include_bytes!("../../web/og.png");
const OG_SCOREBOARD_PNG: &[u8] = include_bytes!("../../web/og-scoreboard.png");
/// Stylesheets and scripts: short enough that a deploy shows within minutes.
const ASSET_CACHE: &str = "public, max-age=300";
const ICON_512_PNG: &[u8] = include_bytes!("../../web/icon-512.png");
const ICON_CACHE: &str = "public, max-age=86400";
const SHOWCASE_CACHE_MS: u128 = 15_000;

#[derive(Clone)]
pub struct HttpConfig {
    pub public_url: String,
    pub admin_token: Option<String>,
    pub spectator_key: Option<String>,
    pub open_registration: bool,
    pub allowed_hosts: Vec<String>,
    /// Where online backups go (None = backups disabled).
    pub backup_dir: Option<std::path::PathBuf>,
}

#[derive(Clone)]
struct AppState {
    game: Game,
    cfg: Arc<HttpConfig>,
    /// Spectator state computed at most once per `SPECTATOR_CACHE_MS` and shared by
    /// every viewer, so dashboard load does not scale with audience size.
    spec_cache: Arc<tokio::sync::Mutex<Option<CachedState>>>,
    /// Public showcase (`af_core::public::showcase`), cached the same way.
    showcase: Arc<tokio::sync::Mutex<Option<CachedState>>>,
    /// The hour-old public fleet (`Game::public_fleet`) and the scoreboard rows, cached the same way.
    galaxy: Arc<tokio::sync::Mutex<Option<CachedState>>>,
    scoreboard: Arc<tokio::sync::Mutex<Option<CachedState>>>,
    /// The last day of calls reduced to public signals per captain, refreshed every few minutes.
    signals: Arc<tokio::sync::Mutex<Option<CachedSignals>>>,
    /// Share cards, analytics and verification for the public pages (see `site`).
    site: Arc<crate::site::Site>,
    /// The live one-line summary written into the home and scoreboard pages, refreshed every minute.
    live: Arc<tokio::sync::Mutex<Option<CachedLive>>>,
}

const SPECTATOR_CACHE_MS: u128 = 1_500;

/// (computed at, serialized spectator state).
type CachedState = (std::time::Instant, Arc<String>);
/// (computed at, the live summary for the home and scoreboard pages).
type CachedLive = (std::time::Instant, Arc<crate::site::Live>);
/// (computed at, public call signals per captain).
type CachedSignals = (std::time::Instant, Arc<std::collections::BTreeMap<u32, serde_json::Value>>);

pub fn router(game: Game, mut cfg: HttpConfig) -> Router {
    // A blank admin token switches the operator API off; it never matches an empty key.
    cfg.admin_token = cfg.admin_token.filter(|t| !t.trim().is_empty());
    let cfg = Arc::new(cfg);
    let mut mcp = GameMcp::new(game.clone(), None, cfg.public_url.clone());
    mcp.open_registration = cfg.open_registration;
    mcp.sysop = cfg
        .admin_token
        .clone()
        .map(|token| Arc::new(crate::sysop_mcp::SysopAccess { token, cfg: cfg.clone() }));
    let mut hosts = vec!["localhost".to_string(), "127.0.0.1".to_string(), "::1".to_string()];
    hosts.extend(cfg.allowed_hosts.iter().cloned());
    let http_cfg = StreamableHttpServerConfig::default()
        .with_allowed_hosts(hosts)
        .with_max_request_body_bytes(usize::MAX)
        .with_sse_keep_alive(Some(Duration::from_secs(15)));
    let svc = StreamableHttpService::new(move || Ok(mcp.clone()), Arc::new(LocalSessionManager::default()), http_cfg);
    let cfg_public_url = cfg.public_url.clone();
    let state = AppState {
        game,
        cfg,
        spec_cache: Arc::new(tokio::sync::Mutex::new(None)),
        showcase: Arc::new(tokio::sync::Mutex::new(None)),
        galaxy: Arc::new(tokio::sync::Mutex::new(None)),
        scoreboard: Arc::new(tokio::sync::Mutex::new(None)),
        signals: Arc::new(tokio::sync::Mutex::new(None)),
        site: Arc::new(crate::site::Site::from_env(&cfg_public_url)),
        live: Arc::new(tokio::sync::Mutex::new(None)),
    };
    // Routes are derived from registered definitions and authored Rust modules.
    // Private documentation never enters this public route collection.
    let mut documentation = Router::new();
    for path in crate::documentation::public_paths() {
        documentation = documentation.route(&format!("/{path}"), get(documentation_txt));
    }
    for (alias, _) in crate::documentation::ALIASES {
        documentation = documentation.route(&format!("/{alias}"), get(documentation_txt));
    }
    Router::new()
        .merge(documentation)
        .route("/", get(page_home))
        .route("/watch", get(page_watch))
        .route("/connect", get(page_connect))
        .route("/scoreboard", get(page_scoreboard))
        .route("/sitemap.xml", get(sitemap_xml))
        .route("/site.webmanifest", get(webmanifest))
        .route(
            "/og.png",
            get(|| async { ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, ICON_CACHE)], OG_PNG) }),
        )
        .route(
            "/og-scoreboard.png",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, ICON_CACHE)],
                    OG_SCOREBOARD_PNG,
                )
            }),
        )
        .route(
            "/scoreboard.js",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "application/javascript"),
                        (header::CACHE_CONTROL, ASSET_CACHE),
                    ],
                    SCOREBOARD_JS,
                )
            }),
        )
        .route(
            "/galaxy3d.js",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "application/javascript"),
                        (header::CACHE_CONTROL, ASSET_CACHE),
                    ],
                    GALAXY3D_JS,
                )
            }),
        )
        .route(
            "/watch.js",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "application/javascript"),
                        (header::CACHE_CONTROL, ASSET_CACHE),
                    ],
                    WATCH_JS,
                )
            }),
        )
        .route(
            "/watch.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css"), (header::CACHE_CONTROL, ASSET_CACHE)],
                    WATCH_CSS,
                )
            }),
        )
        .route(
            "/landing.js",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "application/javascript"),
                        (header::CACHE_CONTROL, ASSET_CACHE),
                    ],
                    LANDING_JS,
                )
            }),
        )
        .route(
            "/landing.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css"), (header::CACHE_CONTROL, ASSET_CACHE)],
                    LANDING_CSS,
                )
            }),
        )
        .route(
            "/connect.js",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "application/javascript"),
                        (header::CACHE_CONTROL, ASSET_CACHE),
                    ],
                    CONNECT_JS,
                )
            }),
        )
        .route(
            "/favicon.svg",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, ICON_CACHE)],
                    FAVICON_SVG,
                )
            }),
        )
        .route(
            "/favicon.ico",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, ICON_CACHE)],
                    FAVICON_32_PNG,
                )
            }),
        )
        .route(
            "/favicon-32.png",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, ICON_CACHE)],
                    FAVICON_32_PNG,
                )
            }),
        )
        .route(
            "/apple-touch-icon.png",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, ICON_CACHE)],
                    APPLE_TOUCH_ICON_PNG,
                )
            }),
        )
        .route(
            "/icon-512.png",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, ICON_CACHE)],
                    ICON_512_PNG,
                )
            }),
        )
        .route("/sysop.txt", get(private_documentation_txt))
        .route("/robots.txt", get(robots_txt))
        .route("/api/public/showcase", get(public_showcase))
        .route("/api/public/galaxy", get(public_galaxy))
        .route("/api/public/scoreboard", get(public_scoreboard))
        .route("/api/public/history", get(public_history))
        .route("/api/public/seasons", get(public_seasons))
        .route("/api/public/constitution", get(public_constitution))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/info", get(info))
        .route("/api/register", post(register))
        .route("/api/recover", post(recover_rest))
        .route("/api/feedback", post(feedback_rest))
        .route("/api/spectator/map", get(spec_map))
        .route("/api/spectator/state", get(spec_state))
        .route("/api/spectator/events", get(spec_events))
        .route("/api/spectator/player", get(spec_player))
        .route("/api/admin/{action}", post(admin))
        .route("/sysop", get(|| async { Html(SYSOP_HTML) }))
        .route(
            "/sysop.js",
            get(|| async { ([(header::CONTENT_TYPE, "text/javascript")], SYSOP_JS) }),
        )
        .route(
            "/sysop-map.js",
            get(|| async { ([(header::CONTENT_TYPE, "text/javascript")], SYSOP_MAP_JS) }),
        )
        .route("/sysop.css", get(|| async { ([(header::CONTENT_TYPE, "text/css")], SYSOP_CSS) }))
        .route("/api/sysop/login", post(sysop_login))
        .route("/api/sysop/logout", post(sysop_logout))
        .route("/api/sysop/me", get(sysop_me))
        .route("/api/sysop/state", get(sysop_state))
        .route("/api/sysop/map", get(sysop_map))
        .route("/api/sysop/events", get(sysop_events))
        .route("/api/sysop/player", get(sysop_player))
        .route("/api/sysop/notes", get(sysop_notes))
        .route("/api/sysop/memory", get(sysop_memory))
        .route("/api/sysop/journal", get(sysop_journal))
        .route("/api/sysop/activity", get(sysop_activity))
        .route("/api/sysop/activity/seasons", get(sysop_activity_seasons))
        .route("/api/sysop/calls", get(sysop_calls))
        .route("/api/sysop/radio", get(sysop_radio))
        .route("/api/sysop/feedback", get(sysop_feedback))
        .route("/api/sysop/acks", get(sysop_acks_get).put(sysop_acks_put))
        .route("/api/sysop/rules", get(sysop_rules))
        .route("/api/sysop/constitution", get(sysop_constitution))
        .route("/api/sysop/history", get(sysop_history))
        .route("/api/sysop/history/season", get(sysop_history_season))
        .route("/api/sysop/history/day", get(sysop_history_day))
        .route("/api/sysop/history/compare", get(sysop_history_compare))
        .route("/api/sysop/history/activity", get(sysop_history_activity))
        .route("/api/sysop/history/removed", get(sysop_history_removed))
        .route("/api/sysop/action/{action}", post(sysop_action))
        .route("/api/sysop/backup", get(sysop_backup))
        .merge(
            Router::new()
                .route_service("/mcp", svc.clone())
                // A catch-all, so a token containing '/' (the operator token is base64) still routes.
                .route_service("/mcp/{*token}", svc)
                .layer(axum::middleware::from_fn_with_state(state.clone(), mcp_transport_log)),
        )
        .layer(DefaultBodyLimit::disable())
        .with_state(state)
}

/// Log MCP requests the transport refuses before any tool runs (no session, a stale session,
/// a bad header), with the JSON-RPC method when there is one. Successful requests are logged by
/// the tool handler instead.
async fn mcp_transport_log(State(s): State<AppState>, req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let started = std::time::Instant::now();
    let (parts, body) = req.into_parts();
    let verb = parts.method.to_string();
    let (rpc_method, body) = if parts.method == axum::http::Method::POST {
        match axum::body::to_bytes(body, usize::MAX).await {
            Ok(bytes) => {
                let m = serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .and_then(|v| v["method"].as_str().map(String::from));
                (m, axum::body::Body::from(bytes))
            }
            Err(_) => (None, axum::body::Body::empty()),
        }
    } else {
        (None, body)
    };
    let token = crate::server::GameMcp::token_from(&parts);
    let session = parts.headers.get("mcp-session-id").and_then(|v| v.to_str().ok()).map(String::from);
    let resp = next.run(axum::extract::Request::from_parts(parts, body)).await;
    let status = resp.status();
    if status.is_client_error() || status.is_server_error() {
        let who = match &token {
            Some(t) => s.game.auth_role(t).await,
            None => None,
        };
        let role = match who {
            Some((_, true)) => "owner",
            Some((_, false)) => "captain",
            None => "anon",
        };
        let tool = rpc_method.unwrap_or(verb);
        // A 404 on a session id: the server restarted and the session is gone; the client reconnects.
        let code = if status == StatusCode::NOT_FOUND && session.is_some() {
            "session_expired".to_string()
        } else {
            format!("http_{}", status.as_u16())
        };
        let mut c = Call::new("http", role, &tool, who.map(|w| w.0))
            .failed(&code)
            .session(session.as_deref());
        c.ms = started.elapsed().as_millis() as u64;
        s.game.record(c);
    }
    resp
}

/// The public showcase for the home page (live, positions binned into regions).
/// A JSON body computed at most once per `SHOWCASE_CACHE_MS` and shared by every viewer.
async fn cached_json<F, Fut>(cache: &tokio::sync::Mutex<Option<CachedState>>, make: F) -> Response
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = serde_json::Value>,
{
    let mut c = cache.lock().await;
    let body = match c.as_ref() {
        Some((at, body)) if at.elapsed().as_millis() < SHOWCASE_CACHE_MS => body.clone(),
        _ => {
            let body = Arc::new(serde_json::to_string(&make().await).unwrap_or_default());
            *c = Some((std::time::Instant::now(), body.clone()));
            body
        }
    };
    drop(c);
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "public, max-age=15"),
        ],
        body.as_ref().clone(),
    )
        .into_response()
}

/// The galaxy an hour ago: every active captain's star and last jump, unlabelled (home page).
async fn public_galaxy(State(s): State<AppState>) -> Response {
    let g = s.game.clone();
    cached_json(&s.galaxy, || async move { g.public_fleet().await }).await
}

/// Per-captain rows for the scoreboard page, with each captain's last day of calls reduced to
/// public signals: the platform its MCP client reports (when known), calls and refused calls.
async fn public_scoreboard(State(s): State<AppState>) -> Response {
    let g = s.game.clone();
    let signals = s.signals.clone();
    cached_json(&s.scoreboard, || async move {
        let sig = {
            let mut c = signals.lock().await;
            match c.as_ref() {
                Some((at, v)) if at.elapsed() < Duration::from_secs(300) => v.clone(),
                _ => {
                    let since = crate::game::now_ms().saturating_sub(86_400_000);
                    let calls = g.activity(None, since, i64::MAX as usize).await;
                    let v = Arc::new(crate::activity::public_signals(&calls));
                    *c = Some((std::time::Instant::now(), v.clone()));
                    v
                }
            }
        };
        let (mut board, ids) = g
            .read(|w| {
                let ids: std::collections::BTreeMap<String, u32> = w.players.values().map(|p| (p.name.clone(), p.id)).collect();
                (af_core::public::scoreboard(w), ids)
            })
            .await;
        if let Some(rows) = board["rows"].as_array_mut() {
            for r in rows {
                let id = r["name"].as_str().and_then(|n| ids.get(n));
                let s = id.and_then(|id| sig.get(id));
                let seen = s.and_then(|s| s["seen_as"].as_str()).map(str::to_string);
                r["seen_as"] = json!(seen);
                r["verified"] = json!(seen.as_deref().is_some_and(|x| Some(x) == r["harness"].as_str()));
                r["calls_24h"] = s.map(|s| s["calls_24h"].clone()).unwrap_or(json!(0));
                r["refused_24h"] = s.map(|s| s["refused_24h"].clone()).unwrap_or(json!(0));
            }
        }
        board
    })
    .await
}

/// The scoreboard's hourly trend this season (see `history`).
async fn public_history(State(s): State<AppState>) -> Response {
    let v = s.game.public_history();
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "public, max-age=60"),
        ],
        v.to_string(),
    )
        .into_response()
}

/// Finished seasons' final scoreboards, newest first (see `history`).
async fn public_seasons(State(s): State<AppState>) -> Response {
    let v = s.game.public_seasons();
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "public, max-age=60"),
        ],
        v.to_string(),
    )
        .into_response()
}

async fn public_showcase(State(s): State<AppState>) -> Response {
    let mut cache = s.showcase.lock().await;
    let body = match cache.as_ref() {
        Some((at, body)) if at.elapsed().as_millis() < SHOWCASE_CACHE_MS => body.clone(),
        _ => {
            let v = s.game.read(af_core::public::showcase).await;
            let body = Arc::new(serde_json::to_string(&v).unwrap_or_default());
            *cache = Some((std::time::Instant::now(), body.clone()));
            body
        }
    };
    drop(cache);
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "public, max-age=15"),
        ],
        body.as_ref().clone(),
    )
        .into_response()
}

/// Public documentation is assembled from live configuration and Rust modules.
async fn documentation_txt(State(s): State<AppState>, uri: Uri) -> Response {
    document_response(&s, uri.path().trim_start_matches('/')).await
}

/// Every page is rendered from the live rules and world, read once per request.
async fn document_response(s: &AppState, path: &str) -> Response {
    let (url, open) = (s.cfg.public_url.clone(), s.cfg.open_registration);
    let ctx = s
        .game
        .read(move |w| crate::documentation::DocContext::from_world(w, &url, open))
        .await;
    match crate::documentation::render(path, &ctx) {
        Some(text) => markdown_response(text),
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

fn markdown_response(text: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/markdown; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        text,
    )
        .into_response()
}

/// Isolated from public indexes, manifests, bundles and crawler guidance.
async fn private_documentation_txt(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if !sysop_ok(&s, &headers) {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }
    markdown_response(crate::documentation::private_document())
}

/// The live summary for the home and scoreboard pages, computed at most once a minute.
async fn live_summary(s: &AppState) -> Arc<crate::site::Live> {
    let mut c = s.live.lock().await;
    if let Some((at, l)) = c.as_ref()
        && at.elapsed() < Duration::from_secs(60)
    {
        return l.clone();
    }
    let board = s.game.read(af_core::public::scoreboard).await;
    let l = Arc::new(crate::site::Live::from_board(&board));
    *c = Some((std::time::Instant::now(), l.clone()));
    l
}

fn page(html: String) -> Response {
    ([(header::CACHE_CONTROL, "public, max-age=60")], Html(html)).into_response()
}

async fn page_home(State(s): State<AppState>) -> Response {
    let live = live_summary(&s).await;
    page(crate::site::render(LANDING_HTML, &s.site, &crate::site::HOME, Some(&live)))
}

async fn page_scoreboard(State(s): State<AppState>) -> Response {
    let live = live_summary(&s).await;
    page(crate::site::render(SCOREBOARD_HTML, &s.site, &crate::site::SCOREBOARD, Some(&live)))
}

async fn page_watch(State(s): State<AppState>) -> Response {
    page(crate::site::render(WATCH_HTML, &s.site, &crate::site::WATCH, None))
}

async fn page_connect(State(s): State<AppState>) -> Response {
    page(crate::site::render(CONNECT_HTML, &s.site, &crate::site::CONNECT, None))
}

/// The HTML pages and the public text guides, for search engines.
async fn sitemap_xml(State(s): State<AppState>) -> Response {
    let guides = crate::documentation::public_paths();
    (
        [
            (header::CONTENT_TYPE, "application/xml; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        crate::site::sitemap(&s.cfg.public_url, &guides),
    )
        .into_response()
}

async fn webmanifest() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/manifest+json"),
            (header::CACHE_CONTROL, ICON_CACHE),
        ],
        crate::site::manifest(),
    )
        .into_response()
}

/// Point agents only at public indexes. Never list private documentation.
async fn robots_txt(State(s): State<AppState>) -> Response {
    let url = s.cfg.public_url.trim_end_matches('/');
    let text = format!(
        "# MCP Wars: crawlers welcome.\n# AI agents: sent by a human, set up at {url}/join; on your own, at {url}/agents.txt. Index: {url}/llms.txt\nUser-agent: *\nAllow: /\nAllow: /api/public/\nDisallow: /api/\nDisallow: /mcp\n\nSitemap: {url}/sitemap.xml\n"
    );
    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        text,
    )
        .into_response()
}

async fn info(State(s): State<AppState>) -> Json<Value> {
    Json(s.game.read(query::game_info).await)
}

#[derive(Deserialize)]
struct RegisterBody {
    name: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    harness: Option<String>,
    #[serde(default)]
    autonomous: bool,
    #[serde(default)]
    human: bool,
}

#[derive(Deserialize)]
struct RecoverBody {
    /// Call sign or player id.
    name: String,
    recovery_key: String,
}

/// Replace a lost token with the captain's recovery key (rate-limited like registration).
/// Log a REST registration or recovery (never its arguments or reply: both carry secrets).
fn log_rest(game: &Game, tool: &str, r: Result<af_core::model::PlayerId, &str>, started: std::time::Instant) {
    let mut c = match r {
        Ok(pid) => Call::new("rest", "anon", tool, Some(pid)),
        Err(code) => Call::new("rest", "anon", tool, None).failed(code),
    };
    c.ms = started.elapsed().as_millis() as u64;
    game.record(c);
}

async fn recover_rest(
    State(s): State<AppState>,
    h: HeaderMap,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    Json(b): Json<RecoverBody>,
) -> Response {
    let ip = s.game.registration.client_ip(&h, peer.map(|p| p.0.0));
    if !s.game.registration.allow(&ip) {
        s.game.record(Call::new("rest", "anon", "recover", None).failed("rate_limited"));
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": "Too many attempts from your address; try again later."})),
        )
            .into_response();
    }
    let started = std::time::Instant::now();
    let r = s.game.recover(&b.name, &b.recovery_key).await;
    log_rest(&s.game, "recover", r.as_ref().map(|x| x.0).map_err(|e| e.code.as_str()), started);
    match r {
        Ok((pid, token)) => Json(json!({
            "player_id": pid, "token": token,
            "mcp_url": format!("{}/mcp/{token}", s.cfg.public_url),
            "note": "Your old token no longer works. Store this token in place of it; your recovery key is unchanged.",
            "persist": crate::game::PERSIST_NOTE,
        }))
        .into_response(),
        Err(r) => (StatusCode::BAD_REQUEST, Json(json!({"error": r}))).into_response(),
    }
}

/// Feedback to the operator. With `Authorization: Bearer <captain token>` it comes from that
/// captain; without one (or with an invalid one) it is anonymous and limited per client IP.
/// The interaction log keeps the lengths only, never the text (and no arguments when anonymous).
async fn feedback_rest(
    State(s): State<AppState>,
    h: HeaderMap,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    Json(b): Json<af_core::feedback::FeedbackArgs>,
) -> Response {
    let started = std::time::Instant::now();
    let token = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.trim().to_string())
        .unwrap_or_default();
    let pid = s.game.auth(&token).await;
    if pid.is_none() {
        let ip = s.game.registration.client_ip(&h, peer.map(|p| p.0.0));
        if !s.game.feedback_limit.allow(&ip) {
            s.game
                .record(Call::new("rest", "anon", "send_feedback", None).failed("rate_limited"));
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error": "Too many feedback reports from your address in the last hour; try again later."})),
            )
                .into_response();
        }
    }
    let lengths = json!({"about": {"chars": b.about.chars().count()}, "text": {"chars": b.text.chars().count()}});
    let actor = pid.map(Actor::Player).unwrap_or(Actor::System);
    // A pasted token or recovery key never reaches the journal.
    let mut b = b;
    b.about = crate::activity::redact_secrets(&b.about);
    b.text = crate::activity::redact_secrets(&b.text);
    let r = s.game.exec(actor, Command::Feedback(b)).await;
    let mut c = Call::new("rest", if pid.is_some() { "captain" } else { "anon" }, "send_feedback", pid);
    if pid.is_some() {
        c.args = Some(lengths);
    }
    if let Err(e) = &r {
        c = c.failed(&e.code);
    }
    c.ms = started.elapsed().as_millis() as u64;
    s.game.record(c);
    match r {
        Ok(o) => Json(json!({"ok": true, "id": o.reply["id"], "summary": o.summary})).into_response(),
        Err(r) => (StatusCode::BAD_REQUEST, Json(json!({"error": r}))).into_response(),
    }
}

async fn register(
    State(s): State<AppState>,
    h: HeaderMap,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    Json(b): Json<RegisterBody>,
) -> Response {
    if !s.cfg.open_registration {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Registration is closed; ask the administrator for a token."})),
        )
            .into_response();
    }
    let ip = s.game.registration.client_ip(&h, peer.map(|p| p.0.0));
    if !s.game.registration.allow(&ip) {
        s.game.record(Call::new("rest", "anon", "register", None).failed("rate_limited"));
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": "Too many registrations from your address in the last hour; try again later."})),
        )
            .into_response();
    }
    tracing::info!("registration request for {:?} from {ip}", b.name);
    let kind = if b.human { PlayerKind::Human } else { PlayerKind::Agent };
    let started = std::time::Instant::now();
    let r = s.game.register_full(&b.name, b.model, b.harness, b.autonomous, kind).await;
    log_rest(&s.game, "register", r.as_ref().map(|x| x.0).map_err(|e| e.code.as_str()), started);
    match r {
        Ok((pid, token, owner, recovery)) => Json(json!({
            "player_id": pid, "name": b.name, "token": token, "recovery_key": recovery,
            "persist": crate::game::PERSIST_NOTE,
            "mcp_url": format!("{}/mcp/{token}", s.cfg.public_url),
            "owner_token": owner,
            "owner_mcp_url": format!("{}/mcp/{owner}", s.cfg.public_url),
            "owner_note": "Optional. The owner token is for the human (or supervisor) behind this captain: connect it as a separate MCP server to choose silent or strategy, set the strategy and leave a note. Do not give it to the captain agent.",
        }))
        .into_response(),
        Err(r) => (StatusCode::BAD_REQUEST, Json(json!({"error": r}))).into_response(),
    }
}

#[derive(Deserialize, Default)]
struct KeyQ {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

fn spectator_ok(s: &AppState, q: &KeyQ, h: &HeaderMap) -> bool {
    match &s.cfg.spectator_key {
        None => true,
        Some(k) => q.key.as_deref() == Some(k.as_str()) || h.get("x-spectator-key").and_then(|v| v.to_str().ok()) == Some(k.as_str()),
    }
}

async fn spec_map(State(s): State<AppState>, Query(q): Query<KeyQ>, h: HeaderMap) -> Response {
    if !spectator_ok(&s, &q, &h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(s.game.read(af_core::public::map).await).into_response()
}

/// The constitution as the public sees it: articles and their versions, open and recent
/// proposals with tallies (never who voted), removed articles and past seasons. Text scrubbed.
async fn public_constitution(State(s): State<AppState>, Query(q): Query<KeyQ>, h: HeaderMap) -> Response {
    if !spectator_ok(&s, &q, &h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(s.game.read(af_core::constitution::public_view).await).into_response()
}

async fn spec_state(State(s): State<AppState>, Query(q): Query<KeyQ>, h: HeaderMap) -> Response {
    if !spectator_ok(&s, &q, &h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut cache = s.spec_cache.lock().await;
    let body = match cache.as_ref() {
        Some((at, body)) if at.elapsed().as_millis() < SPECTATOR_CACHE_MS => body.clone(),
        _ => {
            let v = s.game.read(af_core::public::state).await;
            let body = Arc::new(serde_json::to_string(&v).unwrap_or_default());
            *cache = Some((std::time::Instant::now(), body.clone()));
            body
        }
    };
    drop(cache);
    ([(header::CONTENT_TYPE, "application/json")], body.as_ref().clone()).into_response()
}

/// A captain's public profile (by name).
async fn spec_player(State(s): State<AppState>, Query(q): Query<KeyQ>, h: HeaderMap) -> Response {
    if !spectator_ok(&s, &q, &h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let name = q.name.clone().unwrap_or_default();
    // The public profile: standing and public mentions, never the captain's status.
    let v = s.game.read(move |w| af_core::public::player(w, &name)).await;
    match v {
        Some(v) => Json(v).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn spec_events(State(s): State<AppState>, Query(q): Query<KeyQ>, h: HeaderMap) -> Response {
    if !spectator_ok(&s, &q, &h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let last: u64 = h
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    feed_sse(&s.game.public_recent, &s.game.public_feed, last)
}

/// SSE over one of the engine feeds (public = live but filtered, or everything for sysop).
fn feed_sse(
    recent: &std::sync::Mutex<std::collections::VecDeque<FeedItem>>,
    feed: &tokio::sync::broadcast::Sender<FeedItem>,
    last: u64,
) -> Response {
    let backlog: Vec<FeedItem> = {
        let r = recent.lock().unwrap();
        let skip = if last == 0 { r.len().saturating_sub(100) } else { 0 };
        r.iter().skip(skip).filter(|i| i.id > last).cloned().collect()
    };
    let live = BroadcastStream::new(feed.subscribe()).filter_map(|r| async move { r.ok() });
    let stream: std::pin::Pin<Box<dyn Stream<Item = Result<SseEvent, Infallible>> + Send>> =
        Box::pin(stream::iter(backlog).chain(live).map(|item| {
            Ok(SseEvent::default()
                .id(item.id.to_string())
                .event("game")
                .json_data(&item)
                .unwrap_or_default())
        }));
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

async fn admin(
    State(s): State<AppState>,
    axum::extract::Path(action): axum::extract::Path<String>,
    h: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    let Some(expected) = &s.cfg.admin_token else {
        return (StatusCode::FORBIDDEN, "admin API disabled (start the server with --admin-token)").into_response();
    };
    let ok = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim_start_matches("Bearer ").trim() == expected)
        .unwrap_or(false);
    if !ok {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let b = body.map(|Json(v)| v).unwrap_or(json!({}));
    let Some(res) = run_admin(&s.game, &s.cfg, &action, b).await else {
        return (StatusCode::NOT_FOUND, "unknown admin action").into_response();
    };
    match res {
        Ok(v) => Json(v).into_response(),
        Err(r) => (StatusCode::BAD_REQUEST, Json(json!({"error": r}))).into_response(),
    }
}

/// Every sysop/admin write goes through here, and every one of them is a
/// journaled `Command::Admin`, so the journal stays the single source of truth.
/// Returns None for an unknown action.
pub async fn run_admin(game: &Game, cfg: &HttpConfig, action: &str, b: Value) -> Option<Result<Value, af_core::Refusal>> {
    let res = match action {
        "set_player" => {
            let opt = |k: &str| b[k].as_i64();
            game.exec(
                Actor::System,
                Command::Admin(AdminArgs::SetPlayer {
                    player: b["player"].as_u64().unwrap_or(0) as u32,
                    credits: opt("credits"),
                    bank: opt("bank"),
                    ap: opt("ap"),
                    alignment: opt("alignment"),
                    experience: opt("experience"),
                    sector: b["sector"].as_u64().map(|v| v as u32),
                    fighters: opt("fighters"),
                    shields: opt("shields"),
                    end_protection: b["end_protection"].as_bool(),
                    model: b["model"].as_str().map(String::from),
                    harness: b["harness"].as_str().map(String::from),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary, "changed": o.reply["changed"]}))
        }
        "owner_token" => {
            let pid = b["player"].as_u64().unwrap_or(0) as u32;
            let owner = crate::game::new_token();
            game.exec(
                Actor::System,
                Command::Admin(AdminArgs::SetOwnerToken {
                    player: pid,
                    owner_token_hash: crate::game::hash_token(&owner),
                }),
            )
            .await
            .map(|_| json!({"player_id": pid, "owner_token": owner, "owner_mcp_url": format!("{}/mcp/{owner}", cfg.public_url)}))
        }
        "set_rules" => game
            .exec(Actor::System, Command::Admin(AdminArgs::SetRules { patch: b["patch"].clone() }))
            .await
            .map(|o| json!({"summary": o.summary})),
        "mute" => game
            .exec(
                Actor::System,
                Command::Admin(AdminArgs::Mute {
                    player: b["player"].as_u64().unwrap_or(0) as u32,
                    ticks: b["ticks"].as_u64().unwrap_or(0),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary, "muted_until": o.reply["muted_until"]})),
        "spawn_skarn" => game
            .exec(
                Actor::System,
                Command::Admin(AdminArgs::SpawnSkarn {
                    sector: b["sector"].as_u64().map(|v| v as u32),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary, "player_id": o.reply["player_id"]})),
        "place_concessions" => game
            .exec(Actor::System, Command::Admin(AdminArgs::PlaceConcessions))
            .await
            .map(|o| json!({"summary": o.summary, "result": o.reply})),
        "delete_radio" => game
            .exec(
                Actor::System,
                Command::Admin(AdminArgs::DeleteRadio {
                    id: b["id"].as_u64().unwrap_or(0),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary})),
        "ban" => {
            // Lock a pilot out: banned (no token, owner token or recovery key works) and the
            // token replaced with one nobody holds.
            let pid = b["player"].as_u64().unwrap_or(0) as u32;
            game.exec(
                Actor::System,
                Command::Admin(AdminArgs::Ban {
                    player: pid,
                    new_token_hash: crate::game::hash_token(&crate::game::new_token()),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary, "player_id": pid}))
        }
        "unban" => {
            let pid = b["player"].as_u64().unwrap_or(0) as u32;
            game.exec(Actor::System, Command::Admin(AdminArgs::Unban { player: pid }))
                .await
                .map(|o| json!({"summary": o.summary, "player_id": pid}))
        }
        "tick" => {
            let n = b["count"].as_u64().unwrap_or(1).min(10_000);
            for _ in 0..n {
                let _ = game.exec(Actor::System, Command::Tick).await;
            }
            Ok(json!({"ticked": n}))
        }
        "pause" => {
            game.paused.store(true, Ordering::Relaxed);
            Ok(json!({"paused": true}))
        }
        "resume" => {
            game.paused.store(false, Ordering::Relaxed);
            Ok(json!({"paused": false}))
        }
        "snapshot" => {
            game.snapshot().await;
            Ok(json!({"snapshot": true}))
        }
        "backup" => match &cfg.backup_dir {
            Some(dir) => {
                let dest = dir.join(format!("manual-{}.redb", crate::game::now_ms()));
                game.backup(dest.clone())
                    .await
                    .map(|n| json!({"summary": format!("Backup written ({n} bytes)."), "path": dest}))
                    .map_err(|e| af_core::Refusal::new("backup_failed", e))
            }
            None => Err(af_core::Refusal::new(
                "not_configured",
                "Backups are not configured on this server.",
            )),
        },
        "register" => {
            let kind = match b["kind"].as_str() {
                Some("human") => PlayerKind::Human,
                _ => PlayerKind::Agent,
            };
            game.register_full(
                b["name"].as_str().unwrap_or(""),
                b["model"].as_str().map(|m| m.to_string()),
                b["harness"].as_str().map(|m| m.to_string()),
                b["autonomous"].as_bool().unwrap_or(false),
                kind,
            )
            .await
            .map(|(id, token, owner, recovery)| {
                json!({"player_id": id, "token": token, "recovery_key": recovery, "mcp_url": format!("{}/mcp/{token}", cfg.public_url),
                           "owner_token": owner, "owner_mcp_url": format!("{}/mcp/{owner}", cfg.public_url)})
            })
        }
        "grant" => game
            .exec(
                Actor::System,
                Command::Admin(AdminArgs::Grant {
                    player: b["player"].as_u64().unwrap_or(0) as u32,
                    credits: b["credits"].as_i64().unwrap_or(0),
                    ap: b["ap"].as_i64().unwrap_or(0),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary})),
        "season_end" => game
            .exec(
                Actor::System,
                Command::Admin(AdminArgs::SetSeasonEnd {
                    ends_tick: b["ends_tick"].as_u64().unwrap_or(0),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary})),
        "new_season" => game
            .exec(Actor::System, Command::Admin(AdminArgs::NewSeason))
            .await
            .map(|o| json!({"summary": o.summary})),
        // Refuses unless the body says {"confirm": "fresh start"}: it wipes the game for everyone.
        "fresh_start" => {
            if b["confirm"].as_str() != Some("fresh start") {
                return Some(Err(af_core::Refusal::new(
                    "confirm_required",
                    "A fresh start wipes the game for every captain (only registrations and human setup survive; history, the activity log and feedback are kept). Send {\"confirm\": \"fresh start\"} to proceed.",
                )));
            }
            match game.exec(Actor::System, Command::Admin(AdminArgs::FreshStart)).await {
                // History is kept: the engine archived every captain's result first, and the
                // interaction log and its daily summaries stay.
                Ok(o) => {
                    let acks = game.console_put("console:acks", &json!({})).await;
                    Ok(json!({"summary": o.summary, "captains": o.reply["captains"], "activity_kept": true, "alert_acks_cleared": acks}))
                }
                Err(r) => Err(r),
            }
        }
        "end_season" => game
            .exec(Actor::System, Command::Admin(AdminArgs::EndSeason))
            .await
            .map(|o| json!({"summary": o.summary})),
        "layout_depth" => game
            .exec(Actor::System, Command::Admin(AdminArgs::LayoutDepth))
            .await
            .map(|o| json!({"summary": o.summary, "sectors": o.reply["sectors"]})),
        "link_fedspace" => game
            .exec(
                Actor::System,
                Command::Admin(AdminArgs::LinkFedspace {
                    gateways: b["gateways"].as_u64().map(|g| g as u32),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary, "lanes": o.reply["lanes"]})),
        "broadcast" => game
            .exec(
                Actor::System,
                Command::Admin(AdminArgs::Broadcast {
                    message: b["message"].as_str().unwrap_or("").to_string(),
                    pin: b["pin"].as_bool().unwrap_or(false),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary})),
        "message" => game
            .exec(
                Actor::System,
                Command::Admin(AdminArgs::Message {
                    player: b["player"].as_u64().map(|p| p as u32),
                    text: b["text"].as_str().unwrap_or("").to_string(),
                    tell_owner: b["tell_owner"].as_bool().unwrap_or(false),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary, "recipients": o.reply["recipients"]})),
        "create_corp" | "set_corp" | "corp_add_member" | "corp_remove_member" | "corp_invite" | "corp_uninvite" | "disband_corp" => {
            // Corporations and captains may be given by id or by name.
            let corp_q = b["corp"].clone();
            let player_q = b.get("player").or_else(|| b.get("ceo")).cloned().unwrap_or(Value::Null);
            let ceo_q = b["ceo"].clone();
            let (corp, player, ceo) = game
                .read(move |w| {
                    let corp = corp_q.as_u64().map(|c| c as u32).or_else(|| {
                        corp_q
                            .as_str()
                            .and_then(|n| w.corps.values().find(|c| c.name.eq_ignore_ascii_case(n.trim())).map(|c| c.id))
                    });
                    let who = |v: &Value| {
                        v.as_u64()
                            .map(|p| p as u32)
                            .filter(|p| w.players.contains_key(p))
                            .or_else(|| v.as_str().and_then(|n| w.player_by_name(n)).map(|p| p.id))
                    };
                    (corp, who(&player_q), who(&ceo_q))
                })
                .await;
            let need_corp = || corp.ok_or_else(|| af_core::Refusal::new("unknown_corp", "No corporation with that id or name."));
            let need_player = || player.ok_or_else(|| af_core::Refusal::new("unknown_player", "No captain with that id or name."));
            let args = match action {
                "create_corp" => need_player().map(|p| AdminArgs::CreateCorp {
                    name: b["name"].as_str().unwrap_or("").to_string(),
                    ceo: p,
                    motto: b["motto"].as_str().map(String::from),
                }),
                "set_corp" => need_corp().and_then(|c| {
                    if !b["ceo"].is_null() && ceo.is_none() {
                        return Err(af_core::Refusal::new("unknown_player", "No captain with that id or name for ceo."));
                    }
                    Ok(AdminArgs::SetCorp {
                        corp: c,
                        name: b["name"].as_str().map(String::from),
                        motto: b["motto"].as_str().map(String::from),
                        ceo,
                        treasury: b["treasury"].as_i64(),
                    })
                }),
                "disband_corp" => need_corp().map(|c| AdminArgs::DisbandCorp { corp: c }),
                _ => need_corp().and_then(|c| {
                    need_player().map(|p| match action {
                        "corp_add_member" => AdminArgs::CorpAddMember { corp: c, player: p },
                        "corp_remove_member" => AdminArgs::CorpRemoveMember { corp: c, player: p },
                        "corp_invite" => AdminArgs::CorpInvite { corp: c, player: p },
                        _ => AdminArgs::CorpUninvite { corp: c, player: p },
                    })
                }),
            };
            match args {
                Err(e) => Err(e),
                Ok(args) => game
                    .exec(Actor::System, Command::Admin(args))
                    .await
                    .map(|o| json!({"summary": o.summary, "corp": o.reply["corp"]})),
            }
        }
        "corp_radio" => {
            // The operator posts on one corporation's channel (by id or name); members answer there.
            let corp = b["corp"].as_u64().map(|c| c as u32);
            let name = b["corp"].as_str().map(str::to_lowercase);
            let found = game
                .read(move |w| {
                    w.corps
                        .values()
                        .find(|c| Some(c.id) == corp || name.as_ref().is_some_and(|n| &c.name.to_lowercase() == n))
                        .map(|c| c.id)
                })
                .await;
            let Some(id) = found else {
                return Some(Err(af_core::Refusal::new("unknown_corp", "No corporation with that id or name.")));
            };
            game.exec(
                Actor::System,
                Command::Admin(AdminArgs::CorpRadio {
                    corp: id,
                    text: b["text"].as_str().unwrap_or("").to_string(),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary, "corp": id, "radio_id": o.reply["radio_id"]}))
        }
        "message_corp" => {
            // An operator message to every member of one corporation (one journaled message each).
            let corp = b["corp"].as_u64().map(|c| c as u32);
            let name = b["corp"].as_str().map(str::to_lowercase);
            let found = game
                .read(move |w| {
                    w.corps
                        .values()
                        .find(|c| Some(c.id) == corp || name.as_ref().is_some_and(|n| &c.name.to_lowercase() == n))
                        .map(|c| (c.name.clone(), c.members.clone()))
                })
                .await;
            let Some((cname, members)) = found else {
                return Some(Err(af_core::Refusal::new("unknown_corp", "No corporation with that id or name.")));
            };
            let text = b["text"].as_str().unwrap_or("").to_string();
            let tell = b["tell_owner"].as_bool().unwrap_or(false);
            let mut sent = vec![];
            for m in members {
                let r = game
                    .exec(
                        Actor::System,
                        Command::Admin(AdminArgs::Message {
                            player: Some(m),
                            text: text.clone(),
                            tell_owner: tell,
                        }),
                    )
                    .await;
                match r {
                    Ok(_) => sent.push(m),
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(json!({"summary": format!("Message sent to {} member(s) of {cname}.", sent.len()), "recipients": sent}))
        }
        "recovery_key" => {
            let pid = b["player"].as_u64().unwrap_or(0) as u32;
            let key = crate::game::new_recovery_key();
            game.exec(
                Actor::System,
                Command::Admin(AdminArgs::SetRecovery {
                    player: pid,
                    recovery_hash: crate::game::hash_token(&key),
                }),
            )
            .await
            .map(|_| json!({"summary": "Recovery key issued. Shown once: hand it to the captain privately; it calls recover_access with its call sign and this key.", "player_id": pid, "recovery_key": key}))
        }
        "constitution_strike" => game
            .exec(
                Actor::System,
                Command::Admin(AdminArgs::ConstitutionStrike {
                    article: b["article"].as_u64(),
                    proposal: b["proposal"].as_u64(),
                    reason: b["reason"].as_str().map(String::from),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary})),
        "constitution_propose" => {
            let kind = match b["kind"].as_str().unwrap_or("") {
                "add" => Some(af_core::constitution::ChangeKind::Add),
                "amend" => Some(af_core::constitution::ChangeKind::Amend),
                "repeal" => Some(af_core::constitution::ChangeKind::Repeal),
                _ => None,
            };
            match kind {
                None => Err(af_core::Refusal::new("invalid_args", "kind is required: add, amend or repeal.")),
                Some(kind) => game
                    .exec(
                        Actor::System,
                        Command::Admin(AdminArgs::ConstitutionPropose {
                            kind,
                            text: b["text"].as_str().map(String::from),
                            article: b["article"].as_u64(),
                            note: b["note"].as_str().map(String::from),
                        }),
                    )
                    .await
                    .map(|o| json!({"summary": o.summary, "proposal": o.reply["proposal"], "announcement": o.reply["announcement"]})),
            }
        }
        "constitution_comment" => game
            .exec(
                Actor::System,
                Command::Admin(AdminArgs::ConstitutionComment {
                    proposal: b["proposal"].as_u64().unwrap_or(0),
                    text: b["text"].as_str().unwrap_or("").to_string(),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary, "proposal": o.reply["proposal"], "comments": o.reply["comments"]})),
        "constitution_notice" => game
            .exec(
                Actor::System,
                Command::Admin(AdminArgs::ConstitutionNotice {
                    text: b["text"].as_str().unwrap_or("").to_string(),
                    pin: b["pin"].as_bool().unwrap_or(false),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary, "announcement": o.reply["announcement"], "recipients": o.reply["recipients"]})),
        "resolve_feedback" => game
            .exec(
                Actor::System,
                Command::Admin(AdminArgs::ResolveFeedback {
                    id: b["id"].as_u64().unwrap_or(0),
                    note: b["note"].as_str().map(String::from),
                    reopen: b["reopen"].as_bool().unwrap_or(false),
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary, "id": o.reply["id"], "status": o.reply["status"]})),
        "remove_player" => game
            .exec(
                Actor::System,
                Command::Admin(AdminArgs::RemovePlayer {
                    player: b["player"].as_u64().unwrap_or(0) as u32,
                }),
            )
            .await
            .map(|o| json!({"summary": o.summary})),
        "rotate_token" => {
            let pid = b["player"].as_u64().unwrap_or(0) as u32;
            let token = crate::game::new_token();
            game.exec(
                Actor::System,
                Command::Admin(AdminArgs::RevokeToken {
                    player: pid,
                    new_token_hash: crate::game::hash_token(&token),
                }),
            )
            .await
            .map(|_| json!({"player_id": pid, "token": token}))
        }
        _ => return None,
    };
    Some(res)
}

// ---------- sysop console API ----------
// Auth: the admin token as a Bearer header, or a session cookie obtained by
// POSTing it to /api/sysop/login (EventSource cannot send headers, and the
// admin token should never sit in a URL).

const SYSOP_COOKIE: &str = "af_sysop";
/// Sysop console sessions last 30 days.
const SYSOP_SESSION_MS: u64 = 30 * 24 * 3_600_000;

/// HMAC-SHA256, hex.
fn hmac_hex(key: &[u8], msg: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |b: u8| k.iter().map(|x| x ^ b).collect::<Vec<u8>>();
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(msg).finalize();
    hex::encode(Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize())
}

/// A signed session cookie value: `expiry.nonce.signature`. Stateless, so sessions survive
/// restarts and deploys; rotating the admin token ends them all.
fn sysop_session(admin: &str, expires_ms: u64) -> String {
    let nonce = &crate::game::new_token()[3..19];
    let body = format!("{expires_ms}.{nonce}");
    format!("{body}.{}", hmac_hex(admin.as_bytes(), body.as_bytes()))
}

fn sysop_session_valid(admin: &str, value: &str) -> bool {
    let Some((body, sig)) = value.rsplit_once('.') else { return false };
    let Some(exp) = body.split('.').next().and_then(|e| e.parse::<u64>().ok()) else {
        return false;
    };
    let want = hmac_hex(admin.as_bytes(), body.as_bytes());
    exp > crate::game::now_ms() && sig.len() == want.len() && sig.bytes().zip(want.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}

fn sysop_ok(s: &AppState, h: &HeaderMap) -> bool {
    let Some(expected) = &s.cfg.admin_token else { return false };
    let bearer = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim_start_matches("Bearer ").trim() == expected)
        .unwrap_or(false);
    bearer || cookie(h, SYSOP_COOKIE).is_some_and(|c| sysop_session_valid(expected, &c))
}

fn cookie(h: &HeaderMap, name: &str) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| {
            let (k, v) = kv.trim().split_once('=')?;
            (k == name).then(|| v.to_string())
        })
}

macro_rules! sysop_guard {
    ($s:expr, $h:expr) => {
        if $s.cfg.admin_token.is_none() {
            return (StatusCode::FORBIDDEN, Json(json!({"error": "sysop disabled: start the server with --admin-token"}))).into_response();
        }
        if !sysop_ok(&$s, &$h) {
            return (StatusCode::UNAUTHORIZED, Json(json!({"error": "sysop key required"}))).into_response();
        }
    };
}

#[derive(Deserialize)]
struct LoginBody {
    #[serde(default)]
    key: String,
}

async fn sysop_login(State(s): State<AppState>, Json(b): Json<LoginBody>) -> Response {
    let Some(expected) = &s.cfg.admin_token else {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "sysop disabled: start the server with --admin-token"})),
        )
            .into_response();
    };
    if b.key.trim() != expected {
        // Slow down guessing.
        tokio::time::sleep(Duration::from_millis(600)).await;
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "wrong sysop key"}))).into_response();
    }
    let sid = sysop_session(expected, crate::game::now_ms() + SYSOP_SESSION_MS);
    let secure = if s.cfg.public_url.starts_with("https://") { "; Secure" } else { "" };
    (
        [(
            header::SET_COOKIE,
            format!(
                "{SYSOP_COOKIE}={sid}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}{secure}",
                SYSOP_SESSION_MS / 1000
            ),
        )],
        Json(json!({"ok": true})),
    )
        .into_response()
}

async fn sysop_logout() -> Response {
    (
        [(
            header::SET_COOKIE,
            format!("{SYSOP_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
        )],
        Json(json!({"ok": true})),
    )
        .into_response()
}

async fn sysop_me(State(s): State<AppState>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    let paused = s.game.paused.load(Ordering::Relaxed);
    Json(json!({"ok": true, "paused": paused, "public_url": s.cfg.public_url})).into_response()
}

async fn sysop_state(State(s): State<AppState>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    let paused = s.game.paused.load(Ordering::Relaxed);
    let mut v = s
        .game
        .read(|w| {
            let mut v = spectator::state(w);
            v["live_ms"] = json!(w.now_ms);
            v
        })
        .await;
    v["live"] = json!(true);
    v["paused"] = json!(paused);
    Json(v).into_response()
}

async fn sysop_map(State(s): State<AppState>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    Json(s.game.read(spectator::map).await).into_response()
}

async fn sysop_events(State(s): State<AppState>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    let last: u64 = h
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    feed_sse(&s.game.recent, &s.game.feed, last)
}

/// Everything about one pilot: status, ship, doctrine, private notes, full inbox, chart size.
async fn sysop_player(State(s): State<AppState>, Query(q): Query<KeyQ>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    match sysop_player_view(&s.game, q.name.clone().unwrap_or_default()).await {
        Some(v) => Json(v).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "no such pilot"}))).into_response(),
    }
}

async fn sysop_notes(State(s): State<AppState>, Query(q): Query<crate::sysop_mcp::NotesArgs>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    let r = crate::sysop_mcp::notes(&s.game, q).await;
    if r.ok {
        Json(r.data).into_response()
    } else {
        (StatusCode::BAD_REQUEST, Json(json!({"error": r.text}))).into_response()
    }
}

async fn sysop_memory(State(s): State<AppState>, Query(q): Query<crate::sysop_mcp::MemoryArgs>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    let r = crate::sysop_mcp::memory(&s.game, q).await;
    if r.ok {
        Json(r.data).into_response()
    } else {
        (StatusCode::BAD_REQUEST, Json(json!({"error": r.text}))).into_response()
    }
}

/// One pilot by name or id, as the sysop sees it (None if unknown).
pub async fn sysop_player_view(game: &Game, name: String) -> Option<Value> {
    game.read(move |w| {
        let p = w
            .player_by_name(&name)
            .or_else(|| name.parse().ok().and_then(|id| w.players.get(&id)))?;
        let mut st = query::status(w, p.id).unwrap_or(Value::Null);
        st["notes"] = json!(p.notes);
        st["memory"] = json!(
            p.memory
                .iter()
                .filter_map(|(key, record)| record.versions.last().map(
                    |v| json!({"key": key, "revision": v.revision, "tick": v.tick, "expires_tick": v.expires_tick,
                "archived": v.expires_tick.is_some_and(|t| t <= w.tick), "data": v.data})
                ))
                .collect::<Vec<_>>()
        );
        st["plan"] = query::plan_view(p);
        st["inbox"] = json!(
            p.inbox
                .iter()
                .rev()
                .map(|n| json!({"id": n.id, "tick": n.tick, "at_ms": n.at_ms, "kind": n.kind, "text": n.text, "read": n.read}))
                .collect::<Vec<_>>()
        );
        st["muted_until"] = json!(p.muted_until);
        st["banned"] = json!(p.banned);
        st["aggressor"] = json!(p.aggressor);
        st["created_tick"] = json!(p.created_tick);
        st["last_action_tick"] = json!(p.last_action_tick);
        st["npc"] = json!(p.npc.as_ref().map(|b| format!("{:?}", b.role)));
        st["owner"] = af_core::owner::owner_view(w, p.id)
            .map(|o| json!({"preferences": o["preferences"], "standing_orders": o["standing_orders"], "messages": o["messages"]}))
            .unwrap_or(Value::Null);
        Some(st)
    })
    .await
}

#[derive(Deserialize)]
struct JournalQ {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    include_ticks: Option<bool>,
}

/// The most recent journal entries, newest first (ticks hidden unless asked for).
async fn sysop_journal(State(s): State<AppState>, Query(q): Query<JournalQ>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    Json(sysop_journal_view(&s.game, q.limit.unwrap_or(200), q.include_ticks.unwrap_or(false)).await).into_response()
}

#[derive(Deserialize, Default)]
struct ActivityQ {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    hours: Option<u64>,
    /// A season key, "current" or "earlier" (see `activity_seasons`); takes the place of `hours`.
    #[serde(default)]
    season: Option<String>,
}

async fn sysop_activity(State(s): State<AppState>, Query(q): Query<ActivityQ>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    if let Some(season) = q.season.filter(|k| !k.trim().is_empty()) {
        return history_reply(crate::activity_seasons::view(&s.game, &season, q.name).await);
    }
    match activity_view(&s.game, q.name.filter(|n| !n.trim().is_empty()), q.hours.unwrap_or(24)).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e}))).into_response(),
    }
}

/// The seasons the Activity view can show, newest first (see `activity_seasons::ranges`).
async fn sysop_activity_seasons(State(s): State<AppState>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    history_reply(crate::activity_seasons::seasons(&s.game).await)
}

/// Raw interaction pages reuse the MCP reader and its anchored continuation.
async fn sysop_calls(State(s): State<AppState>, Query(q): Query<crate::sysop_mcp::CallsArgs>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    let reply = crate::sysop_mcp::calls(&s.game, q).await;
    if reply.ok {
        Json(reply.data).into_response()
    } else {
        (StatusCode::NOT_FOUND, Json(json!({"error": reply.text}))).into_response()
    }
}

/// Largest acknowledgement document accepted (bytes of JSON).
const ACKS_MAX: usize = 64 * 1024;

/// The sysop console's alert acknowledgements, shared by every operator session.
async fn sysop_acks_get(State(s): State<AppState>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    Json(json!({"acks": s.game.console_get("acks").await.unwrap_or(json!({}))})).into_response()
}

/// Replace the acknowledgements: `{"acks": {key: {sig, at, until, title}}}`.
async fn sysop_acks_put(State(s): State<AppState>, h: HeaderMap, Json(b): Json<Value>) -> Response {
    sysop_guard!(s, h);
    let acks = &b["acks"];
    if !acks.is_object() || acks.to_string().len() > ACKS_MAX {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "acks must be an object under 64 KB"})),
        )
            .into_response();
    }
    if !s.game.console_put("acks", acks).await {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "could not store acknowledgements"})),
        )
            .into_response();
    }
    Json(json!({"ok": true})).into_response()
}

#[derive(Deserialize, Default)]
pub struct FeedbackQ {
    /// open, resolved or all (default all).
    #[serde(default)]
    pub status: Option<String>,
    /// Reports to return, newest first (default 200, at most 1000).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Only reports with an id below this (the previous page's next_before_id).
    #[serde(default)]
    pub before_id: Option<u64>,
}

/// One page of feedback reports, newest first, with the counts and the next page's cursor
/// (`af_core::feedback::list_json`).
pub async fn feedback_view(game: &Game, q: FeedbackQ) -> Value {
    let limit = q.limit.unwrap_or(200).clamp(1, af_core::feedback::LIST_MAX);
    game.read(move |w| af_core::feedback::list_json(w, q.status.as_deref(), limit, q.before_id))
        .await
}

async fn sysop_feedback(State(s): State<AppState>, Query(q): Query<FeedbackQ>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    Json(feedback_view(&s.game, q).await).into_response()
}

#[derive(Deserialize, Default)]
struct RadioQ {
    #[serde(default)]
    limit: Option<usize>,
    /// JSON cursor from continuation.before, URL encoded by the HTTP client.
    #[serde(default)]
    before: Option<String>,
}

/// Complete original messages with an explicit stable cursor for older pages.
async fn sysop_radio(State(s): State<AppState>, Query(q): Query<RadioQ>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    let before: Option<crate::sysop_mcp::CommsCursor> = match q.before {
        Some(value) => match serde_json::from_str(&value) {
            Ok(cursor) => Some(cursor),
            Err(_) => return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid before cursor"}))).into_response(),
        },
        None => None,
    };
    let messages = s
        .game
        .read(|w| spectator::radio(w, usize::MAX).as_array().cloned().unwrap_or_default())
        .await;
    let mut page = crate::sysop_mcp::message_page(messages, q.limit.unwrap_or(2_000).max(1), before.as_ref());
    page["radio"] = page["messages"].take();
    page.as_object_mut().expect("message page").remove("messages");
    if page["continuation"]["has_more"] == true {
        page["continuation"]["next_query"] = json!({
            "limit": q.limit.unwrap_or(2_000).max(1),
            "before": page["continuation"]["before"].to_string(),
        });
    }
    Json(page).into_response()
}

/// The interaction log over the last `hours` (at most the raw log's retention): one captain (by call sign or id)
/// in full, or every captain's totals plus the calls nobody was logged in for.
pub async fn activity_view(game: &Game, name: Option<String>, hours: u64) -> Result<Value, String> {
    let hours = hours.clamp(1, crate::activity::max_window_hours());
    let now = crate::game::now_ms();
    let since = now.saturating_sub(hours * 3_600_000);
    if let Some(name) = name {
        let n = name.clone();
        let who = game
            .read(move |w| {
                w.player_by_name(&n)
                    .or_else(|| n.parse().ok().and_then(|id| w.players.get(&id)))
                    .map(|p| (p.id, p.name.clone()))
            })
            .await
            .ok_or_else(|| format!("No captain named or numbered {name}."))?;
        let calls = game.activity(Some(who.0), since, i64::MAX as usize).await;
        let mut v = crate::activity::summarize(&calls, now, hours);
        v["player_id"] = json!(who.0);
        v["name"] = json!(who.1);
        v["truncated"] = json!(false);
        return Ok(v);
    }
    let calls = game.activity(None, since, i64::MAX as usize).await;
    let per = crate::activity::per_player(&calls);
    let names: std::collections::BTreeMap<u32, String> = game
        .read(|w| {
            w.players
                .values()
                .filter(|p| p.npc.is_none())
                .map(|p| (p.id, p.name.clone()))
                .collect()
        })
        .await;
    let mut players: Vec<Value> = names
        .iter()
        .map(|(id, n)| {
            let mut v = per
                .get(id)
                .cloned()
                .unwrap_or(json!({"calls": 0, "refused": 0, "last_call_ms": null, "top_tools": []}));
            v["id"] = json!(id);
            v["name"] = json!(n);
            v
        })
        .collect();
    players.sort_by(|a, b| b["last_call_ms"].as_u64().cmp(&a["last_call_ms"].as_u64()));
    let anon: Vec<crate::activity::Call> = calls.iter().filter(|c| c.player.is_none() && c.role != "sysop").cloned().collect();
    Ok(json!({
        "window_hours": hours,
        "calls": calls.len(),
        "truncated": false,
        "players": players,
        "not_logged_in": crate::activity::summarize(&anon, now, hours),
    }))
}

/// The most recent journal entries, newest first (ticks hidden unless asked for).
pub async fn sysop_journal_view(game: &Game, limit: usize, ticks: bool) -> Value {
    let limit = limit.min(1_000);
    let names: std::collections::BTreeMap<u32, String> = game.read(|w| w.players.values().map(|p| (p.id, p.name.clone())).collect()).await;
    let rows: Vec<Value> = {
        let t = game.journal_tail.lock().unwrap();
        t.iter()
            .rev()
            .filter(|e| ticks || !matches!(e.cmd, Command::Tick))
            .take(limit)
            .map(|e| {
                let (actor, who) = match &e.actor {
                    Actor::System => ("system".to_string(), None),
                    Actor::Player(id) => ("player".to_string(), names.get(id).cloned()),
                    Actor::Owner(id) => ("owner".to_string(), names.get(id).cloned()),
                };
                json!({"seq": e.seq, "at_ms": e.at_ms, "actor": actor, "player": who, "cmd": e.cmd})
            })
            .collect()
    };
    json!({"entries": rows})
}

/// Query parameters of `/api/sysop/constitution`.
#[derive(Deserialize, Default)]
struct ConstitutionQ {
    /// A past season (number or season key) from the permanent record; absent or "current" for live.
    #[serde(default)]
    season: Option<String>,
    #[serde(default)]
    proposal: Option<u64>,
    #[serde(default)]
    article: Option<u64>,
}

/// The whole constitution record: articles with their versions, every proposal this season with
/// each vote and every ballot, removed articles, operator notices and the archived seasons. With
/// `season`, that season's constitution from the permanent record; with `proposal` or `article`,
/// just that one. 404 for an unknown season, proposal or article.
async fn sysop_constitution(State(s): State<AppState>, Query(q): Query<ConstitutionQ>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    let r = s
        .game
        .read(move |w| af_core::constitution::sysop_lookup(w, q.proposal, q.article, q.season.as_deref()))
        .await;
    match r {
        Ok(v) => Json(v).into_response(),
        Err(r) => (StatusCode::NOT_FOUND, Json(json!({"error": r.message, "code": r.code}))).into_response(),
    }
}

fn history_reply(r: Result<Value, crate::archive::ViewErr>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err((code, msg)) => (
            StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST),
            Json(json!({"error": msg})),
        )
            .into_response(),
    }
}

/// Query parameters of the history endpoints (each uses its own).
#[derive(Deserialize, Default)]
struct HistoryQ {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    number: Option<String>,
    #[serde(default)]
    season: Option<String>,
    #[serde(default)]
    day: Option<u32>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    epoch: Option<u32>,
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    player: Option<String>,
}

fn season_sel(s: Option<String>) -> Option<crate::archive::SeasonSel> {
    s.map(crate::archive::SeasonSel::Text)
}

/// The permanent record's table of contents (see `history::index`).
async fn sysop_history(State(s): State<AppState>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    Json(crate::archive::index(&s.game).await).into_response()
}

async fn sysop_history_season(State(s): State<AppState>, Query(q): Query<HistoryQ>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    history_reply(crate::archive::season(&s.game, q.key, season_sel(q.number), q.reason, q.epoch).await)
}

async fn sysop_history_day(State(s): State<AppState>, Query(q): Query<HistoryQ>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    let season = match (
        q.key.filter(|k| !k.trim().is_empty()),
        q.season.as_deref().and_then(|x| x.trim().parse().ok()),
    ) {
        (Some(k), _) => Some(crate::archive::DaySeason::Key(k)),
        (None, Some(n)) => Some(crate::archive::DaySeason::Number(n, q.epoch)),
        _ => None,
    };
    let (Some(season), Some(day)) = (season, q.day) else {
        return history_reply(Err((400, "day and a season key (or season number) are required".into())));
    };
    history_reply(crate::archive::day(&s.game, season, day).await)
}

async fn sysop_history_compare(State(s): State<AppState>, Query(q): Query<HistoryQ>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    let season = q.key.filter(|k| !k.trim().is_empty()).or(q.season);
    history_reply(crate::archive::compare(&s.game, season_sel(season), q.by, q.reason, q.epoch).await)
}

async fn sysop_history_activity(State(s): State<AppState>, Query(q): Query<HistoryQ>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    history_reply(crate::archive::activity(&s.game, q.from, q.to, q.player).await)
}

async fn sysop_history_removed(State(s): State<AppState>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    Json(crate::archive::removed(&s.game).await).into_response()
}

async fn sysop_rules(State(s): State<AppState>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    Json(s.game.read(|w| serde_json::to_value(&w.rules).unwrap_or(Value::Null)).await).into_response()
}

async fn sysop_action(
    State(s): State<AppState>,
    axum::extract::Path(action): axum::extract::Path<String>,
    h: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    sysop_guard!(s, h);
    let b = body.map(|Json(v)| v).unwrap_or(json!({}));
    match run_admin(&s.game, &s.cfg, &action, b).await {
        None => (StatusCode::NOT_FOUND, Json(json!({"error": format!("unknown action {action}")}))).into_response(),
        Some(Ok(v)) => Json(v).into_response(),
        Some(Err(r)) => (StatusCode::BAD_REQUEST, Json(json!({"error": r}))).into_response(),
    }
}

/// Download a fresh consistent copy of the universe database (for offsite backups).
async fn sysop_backup(State(s): State<AppState>, h: HeaderMap) -> Response {
    sysop_guard!(s, h);
    let Some(dir) = s.cfg.backup_dir.clone() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "backups are not configured on this server"})),
        )
            .into_response();
    };
    let dest = dir.join("download.redb");
    if let Err(e) = s.game.backup(dest.clone()).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response();
    }
    match tokio::fs::File::open(&dest).await {
        Ok(f) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"mcpwars-{}.redb\"", crate::game::now_ms()),
                ),
            ],
            axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(f)),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

#[cfg(test)]
mod sysop_session_tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231_and_sessions_verify() {
        assert_eq!(
            hmac_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        let s = sysop_session("adm", crate::game::now_ms() + 60_000);
        assert!(sysop_session_valid("adm", &s));
        assert!(!sysop_session_valid("other-key", &s), "a new admin token ends old sessions");
        assert!(!sysop_session_valid("adm", &sysop_session("adm", 1)), "expired");
        let forged = format!("{}x", &s[..s.len() - 1]);
        assert!(!sysop_session_valid("adm", &forged));
    }
}
