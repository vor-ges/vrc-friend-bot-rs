//! vrc-friend-bot (Rust版) の起動。環境変数は Python版と同じ (.env も読む)。
use serenity::all::*;
use std::sync::Arc;
use std::time::Duration;
use vrc_friend_bot_rs::bot::{Config, Core};
use vrc_friend_bot_rs::db::Db;
use vrc_friend_bot_rs::discord::{command_defs, DiscordApi, Handler};
use vrc_friend_bot_rs::stats::{make_tz, Stats};
use vrc_friend_bot_rs::vrc::{Vrc, VrcError};
use vrc_friend_bot_rs::ws::{spawn_worker, ws_loop, WsConfig};

fn env(k: &str) -> Result<String, String> {
    match std::env::var(k) {
        Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => Err(format!("環境変数 {k} が設定されていません (.env を確認してください)")),
    }
}

fn env_id(k: &str) -> Result<u64, String> {
    env(k)?.parse().map_err(|_| format!("環境変数 {k} は数字(ID)で指定してください"))
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn run() -> Result<(), String> {
    let token = env("DISCORD_TOKEN")?;
    let (guild_id, main_ch, owner) = (env_id("GUILD_ID")?, env_id("MAIN_CHANNEL_ID")?, env_id("OWNER_ID")?);
    let (vrc_user, vrc_pass) = (env("VRC_USERNAME")?, env("VRC_PASSWORD")?);
    let ua = std::env::var("VRC_USER_AGENT").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "VRCFriendBot/1.0".into());
    let tz = make_tz(&std::env::var("STATS_TZ").unwrap_or_else(|_| "Asia/Tokyo".into()));

    let db = Db::open("bot.db").map_err(|e| format!("bot.db を開けません: {e}"))?; // Python版と同じファイルをそのまま使える
    let stats = Stats::new(db.c.clone(), tz).map_err(|e| format!("統計テーブルを準備できません: {e}"))?;
    let vrc = Arc::new(Vrc::new(&vrc_user, &vrc_pass, &ua));
    let discord = Arc::new(DiscordApi::new(guild_id, tokio::runtime::Handle::current()));
    let core = Arc::new(Core::new(db, stats, vrc, discord.clone(), Config { main_ch: main_ch as i64, owner: owner as i64 }));

    // Discordに接続する準備 (Python版 intents.default() と違い、必要な GUILDS だけ受け取る)
    let mut client = Client::builder(&token, GatewayIntents::GUILDS)
        .event_handler(Handler::new(core.clone()))
        .await
        .map_err(|e| format!("Discordクライアントを作れません: {e}"))?;
    discord.set_http(client.http.clone());
    let app = client.http.get_current_application_info().await.map_err(|e| format!("Discordに接続できません (DISCORD_TOKEN を確認): {e}"))?;
    client.http.set_application_id(app.id);

    // スラッシュコマンドをサーバーに登録 (同期)
    GuildId::new(guild_id)
        .set_commands(&client.http, command_defs())
        .await
        .map_err(|e| format!("スラッシュコマンドを登録できません (GUILD_ID と Bot の招待スコープ applications.commands を確認): {e}"))?;
    log::info!("slash commands registered");

    // VRChatにログイン (初回は2FAコードを標準入力で聞かれる。以後は .vrc_auth のトークンを再利用)
    let c = core.clone();
    match tokio::task::spawn_blocking(move || c.vrc_login()).await.map_err(|e| e.to_string())? {
        Ok(()) => log::info!("VRChat login OK"),
        Err(VrcError::NeedsReauth) => return Err("VRChatの2FA認証が必要です。ターミナルから直接起動して、認証コードを入力してください (初回のみ)".into()),
        Err(e) => return Err(format!("VRChatにログインできません: {e} (VRC_USERNAME / VRC_PASSWORD を確認)")),
    }

    // 常駐タスク: 生存時刻の記録 / 6時間ごとの再同期 / WebSocket
    let c = core.clone();
    tokio::spawn(async move {
        loop {
            let c2 = c.clone();
            let _ = tokio::task::spawn_blocking(move || c2.stats.heartbeat()).await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
    let c = core.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(6 * 3600)).await; // WebSocketの取りこぼしの保険
            let c2 = c.clone();
            let _ = tokio::task::spawn_blocking(move || c2.sync_friends()).await;
        }
    });
    let tx = spawn_worker(core.clone());
    tokio::spawn(ws_loop(core.clone(), WsConfig::new(&ua), tx));

    let shards = client.shard_manager.clone();
    tokio::select! {
        r = client.start() => r.map_err(|e| format!("Discordとの接続が終了しました: {e}"))?,
        _ = shutdown_signal() => {
            log::info!("shutting down");
            shards.shutdown_all().await;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let _ = dotenvy::dotenv(); // .env があれば読む (無くてもよい)
    // 既定は「自分のログだけ info、依存クレート(serenityなど)は warn 以上」。内部の詳細ログで埋まらないように。
    // RUST_LOG で上書きできる (例: RUST_LOG=debug, RUST_LOG=info,serenity=debug)
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,vrc_friend_bot=info,vrc_friend_bot_rs=info")).init();
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e}");
            eprintln!("エラー: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
