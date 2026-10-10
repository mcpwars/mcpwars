//! Crash safety: a child process journals commands, checkpoints and snapshots
//! in a tight loop until the parent SIGKILLs it. The surviving file must open,
//! have a gap-free journal, and restore (snapshot + replay) to exactly the world
//! a full replay from the base produces.

use std::process::{Command as Proc, Stdio};
use std::time::Duration;

use af_core::cmd::*;
use af_core::engine::{Actor, Entry, apply};
use af_core::model::PlayerKind;
use af_core::rules::Rules;
use af_store::{CHECKPOINT_EVERY, Genesis, Store};

const CHILD_ENV: &str = "AF_CRASH_CHILD_DB";

/// Runs only inside the child process: write forever.
#[test]
fn crash_child() {
    let Ok(path) = std::env::var(CHILD_ENV) else { return };
    let store = Store::open(&path).unwrap();
    let mut w = store
        .init(&Genesis {
            seed: 3,
            sectors: 60,
            rules: Rules::default(),
            created_unix_ms: 0,
            engine: af_core::ENGINE_VERSION.into(),
        })
        .unwrap();
    let mut seq = 0u64;
    let mut i = 0usize;
    loop {
        let cmd = if seq == 0 {
            Command::Register(RegisterArgs {
                name: "Crashy".into(),
                kind: PlayerKind::Agent,
                token_hash: "t".into(),
                model: None,
                owner_token_hash: None,
                harness: None,
                autonomous: false,
                recovery_hash: None,
            })
        } else if i.is_multiple_of(2) {
            Command::Tick
        } else {
            let p = w.players.values().find(|p| p.npc.is_none()).unwrap();
            let warps = w.sector(p.sector).unwrap().warps.clone();
            Command::Warp(WarpArgs {
                destination_sector: warps[i % warps.len()],
            })
        };
        let actor = match cmd {
            Command::Warp(_) => Actor::Player(w.players.values().find(|p| p.npc.is_none()).unwrap().id),
            _ => Actor::System,
        };
        i += 1;
        let e = Entry {
            seq: seq + 1,
            at_ms: seq,
            actor,
            cmd,
        };
        if apply(&mut w, &e).is_ok() {
            seq += 1;
            let cp = seq.is_multiple_of(CHECKPOINT_EVERY).then(|| w.state_hash());
            store.append(&e, cp.as_deref()).unwrap();
            if seq.is_multiple_of(37) {
                store.save_snapshot(&w).unwrap();
            }
        }
    }
}

#[test]
fn killed_writer_leaves_a_consistent_store() {
    if std::env::var(CHILD_ENV).is_ok() {
        return;
    }
    let exe = std::env::current_exe().unwrap();
    for (round, kill_after_ms) in [120u64, 260, 410, 650, 900].into_iter().enumerate() {
        let path = std::env::temp_dir().join(format!("af-crash-{}-{round}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut child = Proc::new(&exe)
            .args(["crash_child", "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, &path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(kill_after_ms));
        child.kill().unwrap(); // SIGKILL: no destructors, no graceful shutdown
        child.wait().unwrap();

        let store = match Store::open(&path) {
            Ok(s) => s,
            Err(e) => panic!("round {round}: store did not reopen after SIGKILL: {e}"),
        };
        if store.genesis().unwrap().is_none() {
            // Killed before genesis committed: an empty but valid database.
            continue;
        }
        let head = store.last_seq().unwrap();
        assert_eq!(store.journal_len().unwrap(), head, "round {round}: journal has gaps");
        let (restored, _) = store.load_world().unwrap();
        assert_eq!(restored.seq, head, "round {round}: restore did not reach the journal head");
        let full = store.replay_from_base().unwrap();
        assert_eq!(
            restored.state_hash(),
            full.state_hash(),
            "round {round}: snapshot restore != base replay"
        );
        eprintln!("round {round}: killed after {kill_after_ms}ms at seq {head}; store consistent");
        drop(store);
        let _ = std::fs::remove_file(&path);
    }
}

/// An online backup taken by the single writer between transactions opens as a
/// complete, verifiable universe.
#[test]
fn online_backup_is_a_consistent_universe() {
    let dir = std::env::temp_dir().join(format!("af-backup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("u.redb");
    let _ = std::fs::remove_file(&path);
    let store = Store::open(&path).unwrap();
    let mut w = store
        .init(&Genesis {
            seed: 5,
            sectors: 60,
            rules: Rules::default(),
            created_unix_ms: 0,
            engine: af_core::ENGINE_VERSION.into(),
        })
        .unwrap();
    for seq in 1..=40u64 {
        let e = Entry {
            seq,
            at_ms: seq,
            actor: Actor::System,
            cmd: Command::Tick,
        };
        apply(&mut w, &e).unwrap();
        store.append(&e, None).unwrap();
    }
    let copy = dir.join("copy.redb");
    store.backup_to(&copy).unwrap();
    drop(store);
    let restored = Store::open(&copy).unwrap();
    let (rw, _) = restored.load_world().unwrap();
    assert_eq!(rw.state_hash(), w.state_hash(), "the backup reproduces the live state");
    assert_eq!(restored.replay_from_base().unwrap().state_hash(), w.state_hash());
    let _ = std::fs::remove_dir_all(&dir);
}
