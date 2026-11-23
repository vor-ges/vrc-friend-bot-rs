//! 他のBot(Python版)の登録の取り込み / 登録チャンネルの実在確認。
use std::collections::HashSet;
use std::sync::Mutex;
use vrc_friend_bot_rs::bot::{Discord, DiscordError};
use vrc_friend_bot_rs::db::{lock, Db};
use vrc_friend_bot_rs::import::*;
use vrc_friend_bot_rs::logic::Embed;

#[derive(Default)]
struct FakeDiscord {
    created: Mutex<Vec<(String, String)>>,
    missing: Mutex<HashSet<i64>>,
    fail_create: Mutex<HashSet<String>>, // このUIDの作成は失敗させる
}
impl Discord for FakeDiscord {
    fn send(&self, _: i64, _: Option<&str>, _: &Embed) -> Result<(), DiscordError> {
        Ok(())
    }
    fn friend_channel(&self, existing: Option<i64>, name: &str, uid: &str) -> Result<i64, DiscordError> {
        if self.fail_create.lock().unwrap().contains(uid) {
            return Err(DiscordError { status: Some(403), message: "Missing Permissions".into() });
        }
        let mut c = self.created.lock().unwrap();
        c.push((uid.to_string(), name.to_string()));
        Ok(existing.unwrap_or(7000 + c.len() as i64))
    }
    fn channel_exists(&self, ch: i64) -> Result<bool, DiscordError> {
        Ok(!self.missing.lock().unwrap().contains(&ch))
    }
}

fn tmp(name: &str) -> String {
    std::env::temp_dir().join(format!("vrcrs-imp-{}-{}-{name}.db", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())).to_string_lossy().to_string()
}

/// Python版の bot.db に相当するファイルを作る
fn python_db() -> String {
    let path = tmp("py");
    let c = rusqlite::Connection::open(&path).unwrap();
    c.execute_batch(
        "CREATE TABLE friends(uid TEXT PRIMARY KEY, name TEXT, channel_id INTEGER, active INTEGER DEFAULT 1, added_at REAL);
         CREATE TABLE bot_settings(k TEXT PRIMARY KEY, v TEXT);
         INSERT INTO friends VALUES('usr_a','Alice',111,1,1000.0),('usr_b','Bob',222,1,2000.0),('usr_c','Gone',333,0,3000.0),('usr_d','日本語 名前',444,1,4000.0);
         INSERT INTO bot_settings VALUES('lang','en'),('mention','0'),('mention:usr_a','1'),('other','x');",
    )
    .unwrap();
    path
}

fn file_bytes(p: &str) -> Vec<u8> {
    std::fs::read(p).unwrap()
}

#[test]
fn plan_shows_what_would_happen_without_changing_anything() {
    let py = python_db();
    let before = file_bytes(&py);
    let bot = Db::memory().unwrap();
    bot.upsert("usr_b", "Bob", 9999).unwrap(); // すでにこのBotに登録済み
    let friends = read_python_friends(&py).unwrap();
    assert_eq!(friends.len(), 4);
    assert_eq!(friends.iter().find(|f| f.uid == "usr_a").unwrap().added_at, Some(1000.0));

    let p = plan(&friends, &bot, false, false);
    let act = |uid: &str| p.iter().find(|i| i.friend.uid == uid).unwrap().action.clone();
    assert_eq!((act("usr_a"), act("usr_b"), act("usr_c"), act("usr_d")), (Action::Create, Action::AlreadyRegistered, Action::SkipRemoved, Action::Create));
    let p = plan(&friends, &bot, true, true); // チャンネルを引き継ぐ + 削除済みも
    let act = |uid: &str| p.iter().find(|i| i.friend.uid == uid).unwrap().action.clone();
    assert_eq!((act("usr_a"), act("usr_b"), act("usr_c")), (Action::Reuse(111), Action::AlreadyRegistered, Action::Reuse(333)));
    assert!(bot.get("usr_a").unwrap().is_none(), "計画を作っただけでは何も登録されない");
    assert_eq!(file_bytes(&py), before, "取り込み元のDBは変更されない");
}

#[test]
fn apply_creates_new_channels_and_is_safe_to_repeat() {
    let py = python_db();
    let bot = Db::memory().unwrap();
    let d = FakeDiscord::default();
    let friends = read_python_friends(&py).unwrap();
    let out = apply(&plan(&friends, &bot, false, false), &bot, &d);
    assert_eq!(out.len(), 3); // Alice, Bob, 日本語 名前 (削除済みのGoneは対象外)
    assert!(out.iter().all(|o| o.result.is_ok()));
    // 新しいチャンネルが作られ、Python版のチャンネルIDは引き継がない
    let a = bot.get("usr_a").unwrap().unwrap();
    assert!(a.active && a.channel_id.unwrap() >= 7001 && a.added_at == Some(1000.0));
    assert!(bot.get("usr_c").unwrap().is_none());
    assert_eq!(d.created.lock().unwrap().iter().find(|c| c.0 == "usr_d").unwrap().1, "日本語-名前");
    // もう一度実行しても、登録済みの人は飛ばされ、チャンネルは増えない
    let n = d.created.lock().unwrap().len();
    assert!(apply(&plan(&friends, &bot, false, false), &bot, &d).is_empty());
    assert_eq!(d.created.lock().unwrap().len(), n);
}

#[test]
fn apply_reuse_keeps_going_when_a_channel_is_missing_or_creation_fails() {
    let py = python_db();
    let bot = Db::memory().unwrap();
    let d = FakeDiscord::default();
    d.missing.lock().unwrap().insert(222); // Bobのチャンネルは存在しない(見えない)
    let friends = read_python_friends(&py).unwrap();
    let out = apply(&plan(&friends, &bot, true, true), &bot, &d);
    let r = |uid: &str| out.iter().find(|o| o.uid == uid).unwrap().result.clone();
    assert_eq!(r("usr_a"), Ok(111)); // 実在するチャンネルはそのまま引き継ぐ
    assert!(r("usr_b").unwrap_err().contains("存在しない"));
    assert!(bot.get("usr_b").unwrap().is_none(), "存在しないチャンネルは登録しない (通知を送り続けない)");
    assert_eq!(r("usr_d"), Ok(444));
    assert!(d.created.lock().unwrap().is_empty()); // 引き継ぎでは作らない
    // 削除済み(--include-removed)は、登録するが通知は止めた状態
    let c = bot.get("usr_c").unwrap().unwrap();
    assert!(!c.active && c.channel_id == Some(333));
    // 作成に失敗した人がいても、他の人は取り込まれる
    let bot2 = Db::memory().unwrap();
    d.fail_create.lock().unwrap().insert("usr_a".into());
    let out = apply(&plan(&friends, &bot2, false, false), &bot2, &d);
    assert!(out.iter().find(|o| o.uid == "usr_a").unwrap().result.as_ref().unwrap_err().contains("Missing Permissions"));
    assert!(bot2.get("usr_a").unwrap().is_none() && bot2.get("usr_b").unwrap().is_some() && bot2.get("usr_d").unwrap().is_some());
}

#[test]
fn settings_are_copied_only_when_missing() {
    let py = python_db();
    let bot = Db::memory().unwrap();
    lock(&bot.c).execute_batch("CREATE TABLE bot_settings(k TEXT PRIMARY KEY, v TEXT); INSERT INTO bot_settings VALUES('mention','1')").unwrap(); // こちらに既にある設定は上書きしない
    let s = read_python_settings(&py).unwrap();
    assert_eq!(s.len(), 3); // lang / mention / mention:usr_a (other は対象外)
    assert_eq!(copy_settings(&s, &bot), 2);
    let get = |k: &str| lock(&bot.c).query_row("SELECT v FROM bot_settings WHERE k=?", [k], |r| r.get::<_, String>(0)).unwrap();
    assert_eq!((get("lang").as_str(), get("mention").as_str(), get("mention:usr_a").as_str()), ("en", "1", "1"));
    assert_eq!(copy_settings(&s, &bot), 0); // 再実行しても増えない
}

#[test]
fn source_must_exist_and_look_like_a_bot_db() {
    assert!(read_python_friends("/no/such/file.db").unwrap_err().contains("見つかりません"));
    assert!(!std::path::Path::new("/no/such/file.db").exists(), "存在しないパスに空のDBを作らない");
    let other = tmp("other");
    rusqlite::Connection::open(&other).unwrap().execute_batch("CREATE TABLE x(a)").unwrap();
    assert!(read_python_friends(&other).unwrap_err().contains("friends テーブルがありません"));
}

#[test]
fn check_reports_channels_that_do_not_exist() {
    let bot = Db::memory().unwrap();
    bot.upsert("usr_a", "Alice", 111).unwrap();
    bot.upsert("usr_b", "Bob", 222).unwrap();
    bot.upsert("usr_c", "Gone", 333).unwrap();
    bot.deactivate("usr_c").unwrap(); // 削除済みは確認しない
    let d = FakeDiscord::default();
    d.missing.lock().unwrap().insert(222);
    let r = check_channels(&bot, &d);
    assert_eq!(r.len(), 2);
    let ok = |uid: &str| matches!(r.iter().find(|(f, _)| f.uid == uid).unwrap().1, Ok(true));
    assert!(ok("usr_a") && !ok("usr_b"));
}
