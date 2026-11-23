//! test_local.py の「WebSocketの配線」(接続のたびに同期 / 切断時刻の記録 / 再接続) の移植。
//! 偽の接続ではなく、手元に立てた本物のWebSocketサーバーに実際に接続して確かめる。
#![cfg(feature = "bot")]
use chrono_tz::Asia::Tokyo;
use futures_util::SinkExt;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;
use vrc_friend_bot_rs::bot::*;
use vrc_friend_bot_rs::db::Db;
use vrc_friend_bot_rs::logic::Embed;
use vrc_friend_bot_rs::stats::Stats;
use vrc_friend_bot_rs::vrc::{FriendEntry, InstanceInfo, UserInfo, VrcError, WorldInfo};
use vrc_friend_bot_rs::ws::{spawn_worker, ws_loop, WsConfig};

#[derive(Default)]
struct FakeVrc {
    friend_calls: AtomicUsize,
}
impl VrcApi for FakeVrc {
    fn login(&self) -> Result<(), VrcError> {
        Ok(())
    }
    fn get_token(&self) -> Result<String, VrcError> {
        Ok("tok".into())
    }
    fn get_user(&self, _: &str) -> Result<UserInfo, VrcError> {
        Err(VrcError::Status(404))
    }
    fn get_world(&self, _: &str) -> Result<WorldInfo, VrcError> {
        Err(VrcError::Status(404))
    }
    fn get_instance(&self, _: &str, _: &str) -> Result<InstanceInfo, VrcError> {
        Err(VrcError::Status(404))
    }
    fn get_group_name(&self, _: &str) -> Result<String, VrcError> {
        Err(VrcError::Status(404))
    }
    fn get_friends_all(&self) -> Result<Vec<FriendEntry>, VrcError> {
        self.friend_calls.fetch_add(1, Ordering::SeqCst);
        Err(VrcError::Transport("boom".into())) // 同期は失敗させて、ws_down_at が保持されることも確かめる
    }
}

struct NoDiscord;
impl Discord for NoDiscord {
    fn send(&self, _: i64, _: Option<&str>, _: &Embed) -> Result<(), DiscordError> {
        Ok(())
    }
    fn friend_channel(&self, e: Option<i64>, _: &str, _: &str) -> Result<i64, DiscordError> {
        Ok(e.unwrap_or(1))
    }
    fn channel_exists(&self, _: i64) -> Result<bool, DiscordError> {
        Ok(true)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnects_syncs_each_time_and_records_disconnect() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(vec![])); // (接続URLのパス, User-Agent)
    let seen2 = seen.clone();
    tokio::spawn(async move {
        let mut n = 0;
        while let Ok((stream, _)) = listener.accept().await {
            n += 1;
            let seen = seen2.clone();
            tokio::spawn(async move {
                let cb = |req: &Request, resp: Response| {
                    let ua = req.headers().get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    seen.lock().unwrap().push((req.uri().to_string(), ua));
                    Ok(resp)
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, cb).await else { return };
                let ev = json!({"type":"friend-online","content":{"userId":"usr_a","platform":"android","location":"private","user":{"status":"active"}}});
                let _ = ws.send(Message::Text(ev.to_string())).await;
                if n == 1 {
                    return; // 1回目の接続は途中で切れる (接続を落とす)
                }
                tokio::time::sleep(Duration::from_secs(3600)).await; // 2回目は維持
            });
        }
    });

    let db = Db::memory().unwrap();
    let stats = Stats::new(db.c.clone(), Tokyo).unwrap();
    let vrc = Arc::new(FakeVrc::default());
    let core = Arc::new(Core::new(db, stats, vrc.clone(), Arc::new(NoDiscord), Config { main_ch: 2, owner: 99 }));
    core.db.upsert("usr_a", "Alice", 111).unwrap();
    assert!(core.state().ws_down_at.is_none());

    let tx = spawn_worker(core.clone());
    let cfg = WsConfig { base_url: format!("ws://{addr}"), user_agent: "TestUA/1 me@example.com".into(), backoff_start: Duration::from_millis(30), backoff_max: Duration::from_millis(100) };
    let task = tokio::spawn(ws_loop(core.clone(), cfg, tx));
    for _ in 0..300 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if seen.lock().unwrap().len() >= 2 && vrc.friend_calls.load(Ordering::SeqCst) >= 2 && core.state().ingame.contains("usr_a") {
            break;
        }
    }
    task.abort();

    let seen = seen.lock().unwrap().clone();
    assert!(seen.len() >= 2, "切断後に再接続した: {seen:?}");
    assert!(seen.iter().all(|(u, ua)| u == "/?authToken=tok" && ua == "TestUA/1 me@example.com"), "{seen:?}"); // トークン付きURL / User-Agent
    assert!(vrc.friend_calls.load(Ordering::SeqCst) >= 2, "接続のたびに同期");
    assert!(core.state().ingame.contains("usr_a"), "イベントは処理される");
    let down = core.state().ws_down_at.expect("切断時刻を記録 (次の同期で使う)");
    assert!(vrc_friend_bot_rs::db::now() - down < 30.0);
}
