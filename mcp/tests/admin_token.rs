//! A blank admin token switches the operator API off: an empty key never opens it.

use af_core::galaxy::{GenesisConfig, generate};
use af_mcp::game::Game;
use af_mcp::http::{HttpConfig, router};
use serde_json::json;

async fn start(admin_token: Option<&str>) -> String {
    let game = Game::spawn(generate(&GenesisConfig::new(5, 120)), None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = router(
        game,
        HttpConfig {
            public_url: url.clone(),
            admin_token: admin_token.map(String::from),
            spectator_key: None,
            open_registration: true,
            allowed_hosts: vec![],
            backup_dir: None,
        },
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blank_admin_token_never_grants_operator_access() {
    let http = reqwest::Client::new();
    for blank in ["", "   "] {
        let url = start(Some(blank)).await;
        let state = http
            .get(format!("{url}/api/sysop/state"))
            .header("Authorization", "Bearer ")
            .send()
            .await
            .unwrap();
        assert_ne!(state.status().as_u16(), 200, "sysop state with a blank token {blank:?}");
        let admin = http
            .post(format!("{url}/api/admin/broadcast"))
            .header("Authorization", "Bearer ")
            .json(&json!({"text": "hi"}))
            .send()
            .await
            .unwrap();
        assert_eq!(admin.status().as_u16(), 403, "admin API with a blank token {blank:?}");
        let login = http
            .post(format!("{url}/api/sysop/login"))
            .json(&json!({"key": ""}))
            .send()
            .await
            .unwrap();
        assert_eq!(login.status().as_u16(), 403, "sysop login with a blank token {blank:?}");
    }
    // A real token still works.
    let url = start(Some("adm")).await;
    let ok = http.get(format!("{url}/api/sysop/state")).bearer_auth("adm").send().await.unwrap();
    assert_eq!(ok.status().as_u16(), 200);
}
