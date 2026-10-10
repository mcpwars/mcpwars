//! A lost wait reply must not hide acknowledged messages from recovery.

use af_core::Command;
use af_core::cmd::AdminArgs;
use af_core::engine::Actor;
use af_core::galaxy::{GenesisConfig, generate};
use af_core::model::PlayerKind;
use af_mcp::{game::Game, tools};
use serde_json::json;

#[tokio::test]
async fn discarded_wait_reply_recovers_complete_messages_and_remaining_pages() {
    let game = Game::spawn(generate(&GenesisConfig::new(17, 60)), None);
    let (pid, _) = game.register("Wait Reader", Some("test".into()), PlayerKind::Human).await.unwrap();
    let (sender, _) = game.register("Wait Sender", Some("test".into()), PlayerKind::Human).await.unwrap();
    let initial = tools::call(&game, pid, "read_inbox", &json!({})).await;
    assert!(initial.ok);

    let args = json!({"timeout_sec": 5});
    let mut wait = Box::pin(tools::call(&game, pid, "wait_for_event", &args));
    assert!(futures::poll!(&mut wait).is_pending());
    // The first poll enqueues the initial watermark read. This fence ensures it
    // precedes delivery, without a timer race between the waiter and sender.
    game.read(|_| ()).await;
    let original = format!("  opening\n{}\ncomplete ending  ", "星".repeat(10_000));
    for i in 0..25 {
        game.exec(
            Actor::System,
            Command::Admin(AdminArgs::Message {
                player: Some(pid),
                text: format!("Message {i}: {original}"),
                tell_owner: false,
            }),
        )
        .await
        .unwrap();
    }
    let radio = tools::call(
        &game,
        sender,
        "radio_broadcast",
        &json!({"channel": "private", "recipient_id": "Wait Reader", "message": original}),
    )
    .await;
    assert!(radio.ok, "{}", radio.text);
    let waited = tokio::time::timeout(std::time::Duration::from_secs(3), wait).await.unwrap();
    assert!(waited.ok);
    assert_eq!(waited.data["result"]["notices"].as_array().unwrap().len(), 20);
    assert_eq!(waited.data["result"]["radio"][0]["message"]["untrusted_player_text"], original);
    assert_eq!(waited.data["result"]["continuation"]["has_more"], true);
    // Simulate a reply the client never received after the server committed its receipt.
    drop(waited);
    let before = game
        .read(move |w| {
            let p = &w.players[&pid];
            (p.inbox.clone(), p.radio_read_ids.clone(), p.ap)
        })
        .await;
    assert_eq!(before.0.iter().filter(|n| n.kind == "sysop" && !n.read).count(), 5);
    assert!(before.1.contains(&radio.data["result"]["message_id"].as_u64().unwrap()));

    let mut args = json!({"include_read": true, "peek": true, "include_public_radio": false, "limit": 7});
    let mut notices = Vec::new();
    let mut recovered_radio = Vec::new();
    loop {
        let reply = tools::call(&game, pid, "read_inbox", &args).await;
        assert!(reply.ok);
        let result = &reply.data["result"];
        notices.extend(result["notices"].as_array().unwrap().iter().cloned());
        recovered_radio.extend(result["radio"].as_array().unwrap().iter().cloned());
        if !result["continuation"]["has_more"].as_bool().unwrap() {
            break;
        }
        args = result["continuation"]["next_arguments"].clone();
        assert_eq!(args["include_read"], true);
        assert_eq!(args["peek"], true);
    }
    for i in 0..25 {
        assert!(
            notices
                .iter()
                .any(|n| n["text"].as_str().unwrap().contains(&format!("Message {i}: {original}")))
        );
    }
    assert!(recovered_radio.iter().any(|m| m["message"]["untrusted_player_text"] == original));
    let after = game
        .read(move |w| {
            let p = &w.players[&pid];
            (p.inbox.clone(), p.radio_read_ids.clone(), p.ap)
        })
        .await;
    assert_eq!(
        before, after,
        "peeking recovers whole messages without changing receipts or spending AP"
    );
}
