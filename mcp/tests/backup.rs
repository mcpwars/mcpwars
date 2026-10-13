//! `GET /api/sysop/backup` and the backup permit: one backup at a time (409 for the next),
//! a private file per request that is gone once sent, Content-Length and X-Backup-SHA256 on the
//! download, and a download that restores to the live universe. Runs on a redb file always and
//! on Postgres when `AF_TEST_POSTGRES_URL` is set (the production path: an export on its own
//! thread and connection).

use std::path::{Path, PathBuf};
use std::time::Duration;

use af_core::Command;
use af_core::engine::Actor;
use af_core::model::PlayerKind;
use af_mcp::game::{BACKUP_BUSY, Game};
use af_mcp::http::{BACKUP_SHA256_HEADER, HttpConfig, router, run_admin};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn locations(name: &str) -> Vec<String> {
    let file = std::env::temp_dir().join(format!("af-mcp-backup-{name}-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let mut out = vec![file.display().to_string()];
    if let Ok(url) = std::env::var("AF_TEST_POSTGRES_URL")
        && !url.is_empty()
    {
        let schema = format!("t_mcp_backup_{name}_{}", std::process::id());
        let mut c = af_store::pg::connect(&url).unwrap();
        c.batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}"))
            .unwrap();
        let sep = if url.contains('?') { '&' } else { '?' };
        out.push(format!("{url}{sep}options=-csearch_path%3D{schema}"));
    }
    out
}

fn config(dir: &Path) -> HttpConfig {
    HttpConfig {
        public_url: "http://localhost".into(),
        admin_token: Some("adm".into()),
        spectator_key: None,
        open_registration: true,
        allowed_hosts: vec![],
        backup_dir: Some(dir.to_path_buf()),
    }
}

/// A universe with some history: ticks, captains and private notes (the private archive).
async fn universe(loc: &str) -> Game {
    let loc = loc.to_string();
    let (world, store, _) = tokio::task::spawn_blocking(move || af_mcp::open_world(&loc, 7, 60, 3_600_000, false).unwrap())
        .await
        .unwrap();
    let game = Game::spawn(world, Some(store));
    for i in 0..3 {
        let (pid, _) = game
            .register(&format!("Archivist {i}"), Some("test".into()), PlayerKind::Agent)
            .await
            .unwrap();
        let r = af_mcp::tools::call(
            &game,
            pid,
            "write_note",
            &json!({"key": "plan", "value": format!("# Plan {i}\n\nTrade.")}),
        )
        .await;
        assert!(r.data["ok"] != json!(false), "{}", r.data);
    }
    for _ in 0..30 {
        game.exec(Actor::System, Command::Tick).await.unwrap();
    }
    game
}

async fn serve(game: &Game, dir: &Path) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = router(game.clone(), config(dir));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

fn files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect()
}

/// Wait (briefly) until the backup slot is free and the directory holds only `keep` files.
async fn settled(game: &Game, dir: &Path, keep: usize) {
    for _ in 0..200 {
        if files(dir).len() == keep
            && let Some(p) = game.backup_permit()
        {
            drop(p);
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "backup did not settle: files {:?}, slot free {}",
        files(dir),
        game.backup_permit().is_some()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_download_is_one_at_a_time_complete_verifiable_and_leaves_nothing() {
    // The Postgres client is synchronous: set the schemas up off the async runtime.
    let locs = tokio::task::spawn_blocking(|| locations("download")).await.unwrap();
    for (i, loc) in locs.into_iter().enumerate() {
        let dir = std::env::temp_dir().join(format!("af-mcp-backup-dir-{}-{i}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        af_mcp::backup::prepare_dir(&dir).unwrap();
        let game = universe(&loc).await;
        let url = serve(&game, &dir).await;
        let http = reqwest::Client::new();
        let get = || http.get(format!("{url}/api/sysop/backup")).bearer_auth("adm").send();

        // While a backup runs, every other backup is refused and nothing is written.
        let running = game.backup_permit().expect("slot free");
        assert!(game.backup_permit().is_none());
        let r = get().await.unwrap();
        assert_eq!(r.status(), 409);
        let body: Value = r.json().await.unwrap();
        assert_eq!(body["code"], "backup_running", "{body}");
        assert_eq!(body["error"], BACKUP_BUSY);
        let refused = run_admin(&game, &config(&dir), "backup", json!({})).await.unwrap().unwrap_err();
        assert_eq!(refused.code, "backup_running");
        assert_eq!(game.backup(dir.join("x.redb")).await.unwrap_err(), BACKUP_BUSY);
        assert!(files(&dir).is_empty(), "{:?}", files(&dir));
        drop(running);

        // A download carries its length and digest, restores to the live universe, and its file
        // is gone once it has been sent.
        for _ in 0..2 {
            let r = get().await.unwrap();
            assert_eq!(r.status(), 200);
            let len: u64 = r.headers()[reqwest::header::CONTENT_LENGTH].to_str().unwrap().parse().unwrap();
            let sha = r.headers()[BACKUP_SHA256_HEADER].to_str().unwrap().to_string();
            assert_eq!(r.headers()[reqwest::header::CACHE_CONTROL], "no-store");
            let bytes = r.bytes().await.unwrap();
            assert_eq!(bytes.len() as u64, len);
            assert_eq!(hex::encode(Sha256::digest(&bytes)), sha);
            settled(&game, &dir, 0).await;
            let copy = std::env::temp_dir().join(format!("af-mcp-backup-copy-{}-{i}.redb", std::process::id()));
            std::fs::write(&copy, &bytes).unwrap();
            let restored = af_store::Store::open(&copy).unwrap();
            let live = game.read(|w| w.state_hash()).await;
            assert_eq!(restored.load_world().unwrap().0.state_hash(), live);
            assert_eq!(restored.replay_from_base().unwrap().state_hash(), live);
            let notes = restored
                .private_archive(&af_store::private_archive::ArchiveQuery {
                    limit: 100,
                    ..Default::default()
                })
                .unwrap();
            assert!(notes.len() >= 3, "the private archive travels with the backup");
            drop(restored);
            let _ = std::fs::remove_file(&copy);
        }

        // A requester that stops waiting: the copy finishes under its permit, then its file goes.
        let permit = game.backup_permit().unwrap();
        let dest = dir.join("abandoned.redb");
        let waiting = tokio::spawn({
            let (game, dest) = (game.clone(), dest.clone());
            async move { game.backup_with(dest, permit).await }
        });
        waiting.abort();
        settled(&game, &dir, 0).await;

        // The operator's backup action writes a private file it keeps.
        let r = run_admin(&game, &config(&dir), "backup", json!({})).await.unwrap().unwrap();
        let kept = PathBuf::from(r["path"].as_str().unwrap());
        assert!(kept.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&kept).unwrap().permissions().mode() & 0o777, 0o600);
        }
        settled(&game, &dir, 1).await;
        let _ = std::fs::remove_dir_all(&dir);
        if !af_store::pg::is_postgres(&loc) {
            let _ = std::fs::remove_file(&loc);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_without_a_database_is_a_json_error_and_frees_the_slot() {
    let dir = std::env::temp_dir().join(format!("af-mcp-backup-none-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    af_mcp::backup::prepare_dir(&dir).unwrap();
    let game = Game::spawn(af_core::galaxy::generate(&af_core::galaxy::GenesisConfig::new(3, 40)), None);
    let url = serve(&game, &dir).await;
    let r = reqwest::Client::new()
        .get(format!("{url}/api/sysop/backup"))
        .bearer_auth("adm")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 500);
    let body: Value = r.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().starts_with("backup failed"), "{body}");
    settled(&game, &dir, 0).await;
    let _ = std::fs::remove_dir_all(&dir);
}
