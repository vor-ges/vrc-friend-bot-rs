//! VRChat pipeline (WebSocket) の受信と、イベントを順番に処理するワーカー。
//! - 接続のたびにフレンド一覧を同期し(切断中に変わった状態の補正)、切れたら指数バックオフで再接続する。
//! - イベントは専用スレッドで1件ずつ順番に処理する (Python版が `await handle()` で順番に処理するのと同じ)。
use crate::bot::Core;
use crate::db::now;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

pub struct WsConfig {
    /// 既定は wss://pipeline.vrchat.cloud (テストでは手元のサーバーに向ける)
    pub base_url: String,
    pub user_agent: String,
    pub backoff_start: Duration,
    pub backoff_max: Duration,
}

impl WsConfig {
    pub fn new(user_agent: &str) -> Self {
        WsConfig { base_url: "wss://pipeline.vrchat.cloud".into(), user_agent: user_agent.into(), backoff_start: Duration::from_secs(5), backoff_max: Duration::from_secs(300) }
    }
}

/// イベントを1件ずつ処理するワーカースレッド。送信側を返す。
pub fn spawn_worker(core: Arc<Core>) -> mpsc::Sender<Value> {
    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::Builder::new()
        .name("vrc-events".into())
        .spawn(move || {
            for msg in rx {
                // 1件の失敗(panic)で止めない (Python版の `except Exception: log.exception("event error")`)
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| core.handle(&msg))).is_err() {
                    log::error!("event error: handler panicked");
                }
            }
        })
        .expect("spawn event worker");
    tx
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn read_loop(ws: &mut Ws, tx: &mpsc::Sender<Value>) -> Result<(), String> {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    tick.tick().await;
    let mut last_rx = Instant::now();
    loop {
        tokio::select! {
            m = ws.next() => match m {
                None => return Ok(()),
                Some(Err(e)) => return Err(e.to_string()),
                Some(Ok(msg)) => {
                    last_rx = Instant::now();
                    match msg {
                        Message::Text(t) => match serde_json::from_str::<Value>(&t) {
                            Ok(v) => { let _ = tx.send(v); }
                            Err(e) => log::error!("event error: {e}"),
                        },
                        Message::Close(_) => return Ok(()),
                        _ => {}
                    }
                }
            },
            _ = tick.tick() => {
                // 無通信が続いたら切れたものとみなす。pingを送ることでpongの返信も流れる
                if last_rx.elapsed() > Duration::from_secs(90) {
                    return Err("ping timeout".into());
                }
                ws.send(Message::Ping(vec![])).await.map_err(|e| e.to_string())?;
            }
        }
    }
}

async fn connect_once(core: &Arc<Core>, cfg: &WsConfig, tx: &mpsc::Sender<Value>, backoff: &mut Duration) -> Result<(), String> {
    let c = core.clone();
    let token = tokio::task::spawn_blocking(move || c.vrc_token()).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    let redact = |s: String| s.replace(&token, "***"); // 接続URLにトークンが入るので、エラー文に出さない
    let mut req = format!("{}/?authToken={}", cfg.base_url, token).into_client_request().map_err(|e| redact(e.to_string()))?;
    req.headers_mut().insert("User-Agent", cfg.user_agent.parse().map_err(|_| "invalid VRC_USER_AGENT".to_string())?);
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.map_err(|e| redact(e.to_string()))?;
    log::info!("pipeline connected");
    *backoff = cfg.backoff_start;
    let c = core.clone();
    tokio::task::spawn_blocking(move || c.sync_friends()); // 切断中に変わった状態を一覧で補正
    let result = read_loop(&mut ws, tx).await.map_err(redact);
    let mut st = core.state(); // 切断時刻を記録(次の同期で、見えていなかった間の離脱を閉じる位置に使う)
    if st.ws_down_at.is_none() {
        st.ws_down_at = Some(now());
    }
    result
}

/// 接続 -> 受信 -> 切断 -> 待ってから再接続 を繰り返す。
pub async fn ws_loop(core: Arc<Core>, cfg: WsConfig, tx: mpsc::Sender<Value>) {
    let mut backoff = cfg.backoff_start;
    loop {
        if let Err(e) = connect_once(&core, &cfg, &tx, &mut backoff).await {
            log::warn!("ws error: {e} (retry {:.0}s)", backoff.as_secs_f32());
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(cfg.backoff_max);
    }
}
