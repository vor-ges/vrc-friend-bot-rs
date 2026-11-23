//! vrc-friend-bot (Rust版) の起動。環境変数は Python版と同じ (.env も読む)。
//!
//!   vrc-friend-bot                          Botを起動する
//!   vrc-friend-bot check                    登録フレンドのチャンネルが実在するか確認する
//!   vrc-friend-bot import <bot.db> [...]    他のBot(Python版)の登録を取り込む (既定は確認だけ)
use serenity::all::*;
use std::sync::Arc;
use std::time::Duration;
use vrc_friend_bot_rs::bot::{Config, Core};
use vrc_friend_bot_rs::db::{enable_wal, open_conn, Db};
use vrc_friend_bot_rs::discord::{command_defs, CategorySpec, DiscordApi, Handler, DEFAULT_CATEGORY};
use vrc_friend_bot_rs::import;
use vrc_friend_bot_rs::stats::{make_tz, Stats};
use vrc_friend_bot_rs::vrc::{Vrc, VrcError};
use vrc_friend_bot_rs::ws::{spawn_worker, ws_loop, WsConfig};

/// 出力先が閉じられても(`| head` など) panic しない println
macro_rules! say {
    ($($t:tt)*) => {{
        use std::io::Write;
        let _ = writeln!(std::io::stdout(), $($t)*);
    }};
}

const USAGE: &str = "使い方:
  vrc-friend-bot                         Botを起動する
  vrc-friend-bot check                   登録フレンドのチャンネルが実在してBotから見えるか確認する
  vrc-friend-bot import <bot.db> [--apply] [--reuse-channels] [--include-removed]
                                         他のBot(Python版)の登録フレンドを取り込む。
                                         既定は確認だけ (--apply で実行)。元のDBには書き込まない。
      --reuse-channels   元のBotのチャンネルをそのまま引き継ぐ (元のBotを止めてから使う)
      --include-removed  削除済みの登録も取り込む (通知は止めた状態で)";

fn opt_env(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn env(k: &str) -> Result<String, String> {
    opt_env(k).ok_or_else(|| format!("環境変数 {k} が設定されていません (.env を確認してください)"))
}

fn id_of(k: &str, v: &str) -> Result<u64, String> {
    v.parse().map_err(|_| format!("環境変数 {k} は数字(ID)で指定してください"))
}

fn env_id(k: &str) -> Result<u64, String> {
    id_of(k, &env(k)?)
}

/// フレンド用チャンネルを作るカテゴリ: FRIEND_CATEGORY_ID(既存のカテゴリのID) > FRIEND_CATEGORY(名前) > 既定名
fn category() -> Result<CategorySpec, String> {
    if let Some(v) = opt_env("FRIEND_CATEGORY_ID") {
        return Ok(CategorySpec::Id(id_of("FRIEND_CATEGORY_ID", &v)?));
    }
    Ok(CategorySpec::Name(opt_env("FRIEND_CATEGORY").unwrap_or_else(|| DEFAULT_CATEGORY.into())))
}

fn bot_db_path() -> String {
    opt_env("BOT_DB").unwrap_or_else(|| "bot-rs.db".into()) // Python版の bot.db とは別のファイル(登録とチャンネルはBotごと)
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

/// 統計DBの開き方。STATS_DB を指定すると他のBot(Python版)と共有し、既定は読み取り専用 (記録の二重書き込みを避ける)。
fn open_stats(db: &Db, tz: chrono_tz::Tz, bot_db: &str) -> Result<Stats, String> {
    let stats_db = opt_env("STATS_DB");
    let requested = opt_env("STATS_MODE");
    let mode: &str = match requested.as_deref() {
        None if stats_db.is_some() => "readonly",
        None => "record",
        Some("record") => "record",
        Some("readonly") => "readonly",
        Some(m) => return Err(format!("STATS_MODE は record か readonly を指定してください (指定: {m})")),
    };
    let Some(path) = stats_db else {
        if mode == "readonly" {
            return Err("STATS_MODE=readonly には STATS_DB (記録を持つDBのパス) が必要です".into());
        }
        log::info!("統計: 登録DBと同じファイル ({bot_db}) に記録します");
        return Stats::new(db.c.clone(), tz).map_err(|e| format!("統計テーブルを準備できません: {e}"));
    };
    let same = |a: &str, b: &str| std::fs::canonicalize(a).ok().zip(std::fs::canonicalize(b).ok()).map_or(a == b, |(x, y)| x == y);
    if same(&path, bot_db) {
        return Err("STATS_DB と BOT_DB が同じファイルです。登録(friends)は共有せず、別のファイルにしてください".into());
    }
    if mode == "readonly" {
        if !std::path::Path::new(&path).is_file() {
            return Err(format!("STATS_DB が見つかりません: {path}"));
        }
        let c = open_conn(&path, true).map_err(|e| format!("STATS_DB を開けません: {e}"))?;
        log::info!("統計: {path} を読み取り専用で参照します (記録は他のBotが担当)");
        return Stats::new_readonly(c, tz);
    }
    let c = open_conn(&path, false).map_err(|e| format!("STATS_DB を開けません: {e}"))?;
    let _ = enable_wal(&c); // 他のプロセスが同時に読めるように
    let s = Stats::new(c, tz).map_err(|e| format!("統計テーブルを準備できません: {e}"))?;
    if s.foreign_writer_active(180.0).unwrap_or(false) && opt_env("STATS_FORCE").as_deref() != Some("1") {
        return Err(format!(
            "{path} には別のBot (Python版) が今も記録しています。記録の二重書き込み(ログイン回数が2倍になる)を避けるため、起動しません。\
             Python版を止めてから起動するか、STATS_MODE=readonly にしてください (承知の上で続けるなら STATS_FORCE=1)"
        ));
    }
    log::info!("統計: {path} に記録します (STATS_MODE=record)");
    Ok(s)
}

async fn run() -> Result<(), String> {
    let token = env("DISCORD_TOKEN")?;
    let (guild_id, main_ch, owner) = (env_id("GUILD_ID")?, env_id("MAIN_CHANNEL_ID")?, env_id("OWNER_ID")?);
    let (vrc_user, vrc_pass) = (env("VRC_USERNAME")?, env("VRC_PASSWORD")?);
    let ua = opt_env("VRC_USER_AGENT").unwrap_or_else(|| "VRCFriendBot/1.0".into());
    let tz = make_tz(&opt_env("STATS_TZ").unwrap_or_else(|| "Asia/Tokyo".into()));
    let cat = category()?;

    let bot_db = bot_db_path();
    let db = Db::open(&bot_db).map_err(|e| format!("{bot_db} を開けません: {e}"))?; // 登録フレンドと設定 (Botごと)
    let stats = open_stats(&db, tz, &bot_db)?;
    let vrc = Arc::new(Vrc::new(&vrc_user, &vrc_pass, &ua));
    let discord = Arc::new(DiscordApi::new(guild_id, tokio::runtime::Handle::current()).with_category(cat.clone()));
    let core = Arc::new(Core::new(db, stats, vrc, discord.clone(), Config { main_ch: main_ch as i64, owner: owner as i64 }));
    log::info!("登録DB: {bot_db} / フレンド用カテゴリ: {cat:?}");

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
            let _ = tokio::task::spawn_blocking(move || c2.stats.heartbeat()).await; // 読み取り専用のときは何もしない
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

/// check / import 用: Discordへ書き込める窓口だけを作る (ゲートウェイには接続しない)。
fn discord_only() -> Result<Arc<DiscordApi>, String> {
    let (token, guild_id) = (env("DISCORD_TOKEN")?, env_id("GUILD_ID")?);
    let api = Arc::new(DiscordApi::new(guild_id, tokio::runtime::Handle::current()).with_category(category()?));
    api.set_http(Arc::new(Http::new(&token)));
    Ok(api)
}

async fn run_check() -> Result<(), String> {
    let path = bot_db_path();
    let db = Db::open(&path).map_err(|e| format!("{path} を開けません: {e}"))?;
    let api = discord_only()?;
    let results = tokio::task::spawn_blocking(move || import::check_channels(&db, &*api)).await.map_err(|e| e.to_string())?;
    let mut bad = 0;
    for (f, r) in &results {
        let (mark, note) = match r {
            Ok(true) => ("OK ", String::new()),
            Ok(false) => ("NG ", "  ← チャンネルが存在しない、またはBotから見えません".to_string()),
            Err(e) => ("?? ", format!("  ← 確認できません: {e}")),
        };
        if !matches!(r, Ok(true)) {
            bad += 1;
        }
        say!("{mark} {} <#{}>{note}", f.name.as_deref().unwrap_or(&f.uid), f.channel_id.unwrap_or(0));
    }
    say!("\n登録 {} 人 / 問題あり {bad} 人 ({path})", results.len());
    if bad > 0 { Err("存在しないチャンネルに通知を送り続けないよう、上のフレンドを /remove するか、チャンネルを作り直してください".into()) } else { Ok(()) }
}

async fn run_import(args: &[String]) -> Result<(), String> {
    let flag = |f: &str| args.iter().any(|a| a == f);
    let Some(src) = args.iter().find(|a| !a.starts_with("--")) else { return Err(format!("取り込み元の bot.db のパスを指定してください\n\n{USAGE}")) };
    let (apply, reuse, removed) = (flag("--apply"), flag("--reuse-channels"), flag("--include-removed"));
    let path = bot_db_path();
    if std::fs::canonicalize(src).ok().zip(std::fs::canonicalize(&path).ok()).map_or(false, |(a, b)| a == b) {
        return Err("取り込み元と BOT_DB が同じファイルです".into());
    }
    let py = import::read_python_friends(src)?;
    let db = Db::open(&path).map_err(|e| format!("{path} を開けません: {e}"))?;
    let items = import::plan(&py, &db, reuse, removed);
    say!("取り込み元: {src} ({} 件) → 取り込み先: {path}\n", py.len());
    for it in &items {
        let what = match &it.action {
            import::Action::AlreadyRegistered => "登録済み (何もしない)".to_string(),
            import::Action::SkipRemoved => "削除済み (取り込まない)".to_string(),
            import::Action::Create => "新しいチャンネルを作って登録".to_string(),
            import::Action::Reuse(ch) => format!("チャンネル <#{ch}> を引き継いで登録"),
        };
        say!("  {:<24} {what}", it.friend.name.as_deref().unwrap_or(&it.friend.uid));
    }
    let todo = items.iter().filter(|i| matches!(i.action, import::Action::Create | import::Action::Reuse(_))).count();
    if !apply {
        say!("\n確認だけです (何も変更していません)。取り込むのは {todo} 人。実行するには --apply を付けてください。");
        if reuse {
            say!("※ --reuse-channels は、元のBotを止めてから実行してください (同じチャンネルに2つのBotが書き込むため)。");
        }
        return Ok(());
    }
    let api = discord_only()?;
    let db = Arc::new(db);
    let db2 = db.clone();
    let outcomes = tokio::task::spawn_blocking(move || import::apply(&items, &db2, &*api)).await.map_err(|e| e.to_string())?;
    let mut failed = 0;
    for o in &outcomes {
        match &o.result {
            Ok(ch) => say!("  OK  {} → <#{ch}>", o.name),
            Err(e) => {
                failed += 1;
                say!("  NG  {} : {e}", o.name);
            }
        }
    }
    let copied = import::copy_settings(&import::read_python_settings(src)?, &db);
    say!("\n取り込み {} 人 / 失敗 {failed} 人 / 設定のコピー {copied} 件 (言語・メンションで、こちらにまだ無いものだけ)", outcomes.len() - failed);
    if failed > 0 { Err("失敗した人は、原因を直してもう一度実行してください (取り込み済みの人は飛ばされます)".into()) } else { Ok(()) }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let _ = dotenvy::dotenv(); // .env があれば読む (無くてもよい)
    // 既定は「自分のログだけ info、依存クレート(serenityなど)は warn 以上」。内部の詳細ログで埋まらないように。
    // RUST_LOG で上書きできる (例: RUST_LOG=debug, RUST_LOG=info,serenity=debug)
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,vrc_friend_bot=info,vrc_friend_bot_rs=info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        None => run().await,
        Some("check") => run_check().await,
        Some("import") => run_import(&args[1..]).await,
        Some("-h" | "--help" | "help") => {
            say!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("不明なコマンド: {other}\n\n{USAGE}")),
    };
    match r {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e}");
            eprintln!("エラー: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
