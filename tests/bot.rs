//! test_local.py のボット側テスト (イベント処理・コマンド・同期・/add・/stats who・言語・メンション) の移植。
//! Discord と VRChat は偽物(FakeDiscord / FakeVrc)に差し替える。
mod common;
use chrono::TimeZone;
use chrono_tz::Asia::Tokyo;
use common::t;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use vrc_friend_bot_rs::bot::*;
use vrc_friend_bot_rs::db::{lock, Db};
use vrc_friend_bot_rs::i18n;
use vrc_friend_bot_rs::logic::Embed;
use vrc_friend_bot_rs::stats::Stats;
use vrc_friend_bot_rs::vrc::{explain_error, FriendEntry, InstanceInfo, UserInfo, VrcError, WorldInfo};

const OWNER: i64 = 99;
const MAIN: i64 = 2;
const W: &str = "wrld_aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";

// 言語はグローバル状態なので、このファイルのテストは直列に動かし、毎回 ja に戻す
static LANG: Mutex<()> = Mutex::new(());
fn serial() -> MutexGuard<'static, ()> {
    let g = LANG.lock().unwrap_or_else(|e| e.into_inner());
    i18n::set_lang("ja").unwrap();
    g
}

// ---------- 偽物 ----------

#[derive(Default)]
struct FakeVrc {
    world_calls: AtomicUsize,
    instance_calls: AtomicUsize,
    user_calls: AtomicUsize,
    owner_calls: AtomicUsize, // get_user + get_group_name
    friends: Mutex<Vec<FriendEntry>>,
    fail_friends: AtomicBool,
    fail_user: AtomicBool,
    users: Mutex<HashMap<String, Result<UserInfo, u16>>>,
    friend_calls: AtomicUsize,
    gate: Mutex<()>, // 同期の二重実行テスト用: ロックしている間 get_friends_all が止まる
    in_call: AtomicBool,
}

impl VrcApi for FakeVrc {
    fn login(&self) -> Result<(), VrcError> {
        Ok(())
    }
    fn get_token(&self) -> Result<String, VrcError> {
        Ok("tok".into())
    }
    fn get_user(&self, uid: &str) -> Result<UserInfo, VrcError> {
        self.user_calls.fetch_add(1, Ordering::SeqCst);
        self.owner_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_user.load(Ordering::SeqCst) {
            return Err(VrcError::Transport("x".into()));
        }
        match self.users.lock().unwrap().get(uid) {
            Some(Ok(u)) => Ok(u.clone()),
            Some(Err(c)) => Err(VrcError::Status(*c)),
            None => Ok(UserInfo { name: "OwnerUser".into(), status: "active".into(), desc: String::new(), is_friend: false, state: String::new() }),
        }
    }
    fn get_world(&self, _w: &str) -> Result<WorldInfo, VrcError> {
        self.world_calls.fetch_add(1, Ordering::SeqCst);
        Ok(WorldInfo { name: "W".into(), author: "AUTHOR".into(), capacity: Some(32), thumb: None, desc: "d".into() })
    }
    fn get_instance(&self, _w: &str, _i: &str) -> Result<InstanceInfo, VrcError> {
        self.instance_calls.fetch_add(1, Ordering::SeqCst);
        Ok(InstanceInfo { n_users: Some(5), capacity: Some(16) })
    }
    fn get_group_name(&self, _g: &str) -> Result<String, VrcError> {
        self.owner_calls.fetch_add(1, Ordering::SeqCst);
        Ok("MyGroup".into())
    }
    fn get_friends_all(&self) -> Result<Vec<FriendEntry>, VrcError> {
        self.friend_calls.fetch_add(1, Ordering::SeqCst);
        self.in_call.store(true, Ordering::SeqCst);
        let _g = self.gate.lock().unwrap();
        if self.fail_friends.load(Ordering::SeqCst) {
            return Err(VrcError::Transport("boom".into()));
        }
        Ok(self.friends.lock().unwrap().clone())
    }
}

#[derive(Clone, Debug)]
struct Sent {
    ch: i64,
    content: Option<String>,
    embed: Embed,
}

#[derive(Default)]
struct FakeDiscord {
    sent: Mutex<Vec<Sent>>,
    created: Mutex<Vec<(String, String)>>, // (uid, チャンネル名)
    missing: Mutex<std::collections::HashSet<i64>>, // 存在しない(見えない)チャンネル
}

impl Discord for FakeDiscord {
    fn send(&self, ch: i64, content: Option<&str>, e: &Embed) -> Result<(), DiscordError> {
        self.sent.lock().unwrap().push(Sent { ch, content: content.map(str::to_string), embed: e.clone() });
        Ok(())
    }
    fn friend_channel(&self, existing: Option<i64>, name: &str, uid: &str) -> Result<i64, DiscordError> {
        if let Some(e) = existing {
            return Ok(e);
        }
        let mut c = self.created.lock().unwrap();
        c.push((uid.to_string(), name.to_string()));
        Ok(900 + c.len() as i64)
    }
    fn channel_exists(&self, ch: i64) -> Result<bool, DiscordError> {
        Ok(!self.missing.lock().unwrap().contains(&ch))
    }
}

impl FakeDiscord {
    fn titles(&self) -> Vec<String> {
        self.sent.lock().unwrap().iter().map(|s| s.embed.title.clone().unwrap_or_default()).collect()
    }
    fn last(&self) -> Sent {
        self.sent.lock().unwrap().last().cloned().expect("何も送られていない")
    }
    fn n(&self) -> usize {
        self.sent.lock().unwrap().len()
    }
    fn clear(&self) {
        self.sent.lock().unwrap().clear();
    }
}

fn newbot() -> (Core, Arc<FakeVrc>, Arc<FakeDiscord>) {
    let db = Db::memory().unwrap();
    let stats = Stats::new(db.c.clone(), Tokyo).unwrap();
    lock(&db.c).execute("UPDATE stats_meta SET v=? WHERE k='since'", [t(1, 0, 0).to_string()]).unwrap();
    let (v, d) = (Arc::new(FakeVrc::default()), Arc::new(FakeDiscord::default()));
    let core = Core::new(db, stats, v.clone(), d.clone(), Config { main_ch: MAIN, owner: OWNER });
    (core, v, d)
}

fn ev(c: &Core, t: &str, uid: &str, content: Value) {
    let mut m = json!({ "userId": uid });
    for (k, v) in content.as_object().unwrap() {
        m[k] = v.clone();
    }
    c.handle(&json!({ "type": t, "content": m }));
}

fn kinds(c: &Core, uid: &str) -> Vec<(String, Option<String>)> {
    let cn = c.stats.conn();
    let conn = lock(&cn);
    let mut st = conn.prepare("SELECT kind,status FROM activity WHERE uid=? ORDER BY ts,id").unwrap();
    st.query_map([uid], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()
}

fn fields(e: &Embed) -> HashMap<String, String> {
    e.fields.iter().map(|f| (f.name.clone(), f.value.clone())).collect()
}

fn is_jp(s: &str) -> bool {
    s.chars().any(|c| matches!(c, 'ぁ'..='ん' | 'ァ'..='ヶ' | '一'..='龠'))
}

fn uid(n: i64) -> String {
    format!("usr_{n:08x}-0000-0000-0000-000000000000")
}
fn uu(n: i64) -> String {
    format!("usr_{n:08x}-1111-1111-1111-111111111111")
}

fn entry(u: &str, name: &str, ingame: bool, status: &str, loc: Option<&str>) -> FriendEntry {
    FriendEntry {
        uid: u.into(),
        name: name.into(),
        status: status.into(),
        desc: String::new(),
        location: loc.map(str::to_string).unwrap_or_else(|| if ingame { "wrld_x:1~region(jp)".into() } else { "offline".into() }),
        ingame,
    }
}

fn do_add(c: &Core, user: i64, ch: i64, target: &str) -> Reply {
    match c.cmd_add_prepare(user, ch, target) {
        Err(r) => r,
        Ok(u) => c.cmd_add_run(user, &u),
    }
}

// ---------- DB + autocomplete / resolve ----------

#[test]
fn autocomplete_and_resolve() {
    let _g = serial();
    let (c, _, _) = newbot();
    for n in 0..40 {
        c.db.upsert(&uid(n), &if n < 3 { "Alice".to_string() } else { format!("User{n}") }, 1000 + n).unwrap();
    }
    c.db.upsert(&uid(99), "Gone", 5).unwrap();
    c.db.deactivate(&uid(99)).unwrap();
    assert!(c.ac_remove(1, "").is_empty()); // 他人には出さない
    let a = c.ac_remove(OWNER, "");
    assert_eq!(a.len(), 25);
    assert!(a.iter().all(|x| x.name.chars().count() <= 100 && x.value.chars().count() <= 100));
    let a = c.ac_remove(OWNER, "alice");
    assert_eq!(a.len(), 3);
    assert_eq!(a.iter().map(|x| &x.name).collect::<std::collections::HashSet<_>>().len(), 3); // 同名でも区別できる
    assert!(a.iter().all(|x| x.value.starts_with("usr_")));
    assert!(c.ac_remove(OWNER, "gone").is_empty()); // 削除済みは候補に出ない
    assert_eq!(c.resolve_remove(&uid(5)).unwrap().name.as_deref(), Some("User5"));
    assert!(c.resolve_remove(&uid(99)).is_err() && c.resolve_remove(&uid(500)).is_err());
    assert!(c.resolve_remove("alice").unwrap_err().contains("複数"));
    assert_eq!(c.resolve_remove("user7").unwrap().uid, uid(7));
    let lv = vrc_friend_bot_rs::logic::ListView::new(c.db.active().unwrap(), true, None, Default::default());
    assert!(lv.pages == 4 && lv.embed().description.unwrap().chars().count() < 4096);
}

// ---------- インスタンスオーナー表示 ----------

fn embed_for(c: &Core, loc: &str) -> HashMap<String, String> {
    let mut e = Embed::new();
    c.world_embed_fields(Some(loc), &mut e);
    fields(&e)
}

#[test]
fn instance_owner_display() {
    let _g = serial();
    let (c, v, _) = newbot();
    let f = embed_for(&c, &format!("{W}:1~group(grp_y)~groupAccessType(members)~region(jp)"));
    assert_eq!(f["オーナー"], "MyGroup (グループ)");
    assert!(!f.contains_key("作者"));
    let f = embed_for(&c, &format!("{W}:1~hidden(usr_o)~region(jp)"));
    assert_eq!((f["オーナー"].as_str(), f["インスタンス"].as_str(), f["人数"].as_str()), ("OwnerUser", "Friends+ (JP)", "5/16"));
    embed_for(&c, &format!("{W}:1~hidden(usr_o)~region(jp)"));
    assert_eq!(v.owner_calls.load(Ordering::SeqCst), 2); // キャッシュでAPI再呼び出しなし
    c.db.upsert("usr_known", "KnownFriend", 1).unwrap();
    assert_eq!(embed_for(&c, &format!("{W}:1~friends(usr_known)"))["オーナー"], "KnownFriend"); // 登録済みはAPI不要
    assert_eq!(v.owner_calls.load(Ordering::SeqCst), 2);
    assert!(!embed_for(&c, &format!("{W}:1")).contains_key("オーナー")); // Public
    v.fail_user.store(true, Ordering::SeqCst);
    assert_eq!(embed_for(&c, &format!("{W}:1~private(usr_fail)"))["オーナー"], "取得不可");
    assert_eq!(embed_for(&c, "private")["場所"], "Private / 取得不可");
    assert_eq!(embed_for(&c, "wrld_x")["場所"], "wrld_x"); // 解析できない値はそのまま
}

// ---------- ゲーム内/Webの区別 ----------

#[test]
fn ingame_vs_web() {
    let _g = serial();
    let (c, _, d) = newbot();
    c.db.upsert("usr_a", "Alice", 1).unwrap();
    let p = format!("{W}:1~friends(usr_o)~region(jp)");
    let (on, off) = ("🟢 Alice がオンライン", "⚫ Alice がオフライン");
    let case = |label: &str, steps: Vec<(&str, Value)>, expect: Vec<&str>| {
        d.clear();
        c.state().ingame.clear();
        c.state().last_loc.clear();
        for (t, content) in steps {
            ev(&c, t, "usr_a", content);
        }
        assert_eq!(d.titles(), expect, "{label}");
    };
    case("Webだけ(active→offline)", vec![("friend-active", json!({"platform":"web"})), ("friend-offline", json!({}))], vec![]);
    case("ゲーム→offline→offline", vec![("friend-online", json!({"platform":"standalonewindows","location":p})), ("friend-offline", json!({})), ("friend-offline", json!({}))], vec![on, off]);
    case(
        "Web→ゲーム→offline→Web offline",
        vec![("friend-active", json!({"platform":"web"})), ("friend-online", json!({"platform":"android","location":"private"})), ("friend-offline", json!({})), ("friend-offline", json!({}))],
        vec![on, off],
    );
    case("ゲーム終了しWebだけ残る", vec![("friend-online", json!({"platform":"standalonewindows","location":"private"})), ("friend-active", json!({"platform":"web"})), ("friend-offline", json!({}))], vec![on, off]);
    case("friend-onlineがweb", vec![("friend-online", json!({"platform":"web"})), ("friend-offline", json!({}))], vec![]);
    case("location offlineは無視", vec![("friend-location", json!({"location":"offline"})), ("friend-offline", json!({}))], vec![]);

    // active のキーが小文字 userid、content が JSON 文字列でも処理できる
    d.clear();
    c.state().ingame.insert("usr_a".into());
    c.handle(&json!({"type":"friend-active","content": r#"{"userid":"usr_a","platform":"web"}"#}));
    assert_eq!(d.n(), 1);
    assert!(!c.state().ingame.contains("usr_a"));
    c.handle(&json!({"type":"friend-offline","content": "これはJSONではない"})); // 壊れたメッセージは無視
    c.handle(&json!({"type":"friend-offline","content": {"nouser": 1}}));
    assert_eq!(d.n(), 1);

    // 場所だけ届いた(online取りこぼし)場合もゲーム内として復元され、後のofflineが通知される
    d.clear();
    c.state().ingame.clear();
    c.state().last_loc.clear();
    ev(&c, "friend-location", "usr_a", json!({"location": p}));
    ev(&c, "friend-offline", "usr_a", json!({}));
    assert_eq!(d.titles(), ["🚪 Alice が移動", off]);

    // 起動時のseed: state=online ならゲーム内、active/offline ならそうでない
    let u = |state: &str| UserInfo { name: "Alice".into(), status: "active".into(), desc: String::new(), is_friend: true, state: state.into() };
    c.state().ingame.clear();
    c.seed("usr_a", &u("online"));
    assert!(c.state().ingame.contains("usr_a"));
    c.seed("usr_a", &u("active"));
    assert!(!c.state().ingame.contains("usr_a"));
    d.clear();
    c.seed("usr_a", &u("online"));
    ev(&c, "friend-offline", "usr_a", json!({}));
    assert_eq!(d.n(), 1);
}

// ---------- イベント -> 記録 / /stats ----------

#[test]
fn events_record_and_stats_command() {
    let _g = serial();
    let (mut c, v, d) = newbot();
    c.db.upsert("usr_a", "Alice", 111).unwrap();
    c.db.upsert("usr_b", "Bob*", 222).unwrap();
    let e = |c: &Core, t: &str, content: Value| ev(c, t, "usr_a", content);
    e(&c, "friend-active", json!({"platform":"web"}));
    e(&c, "friend-offline", json!({})); // Webだけ -> 記録なし
    e(&c, "friend-update", json!({"user":{"status":"busy"}})); // ゲーム外のステータス変更 -> 記録なし
    assert!(kinds(&c, "usr_a").is_empty());
    e(&c, "friend-online", json!({"platform":"standalonewindows","location":"private","user":{"status":"join me"}}));
    e(&c, "friend-update", json!({"user":{"status":"busy"}}));
    e(&c, "friend-update", json!({"user":{"status":"busy","statusDescription":"x"}})); // 同ステータスは1回
    e(&c, "friend-offline", json!({}));
    e(&c, "friend-offline", json!({}));
    let k = |s: &str, st: Option<&str>| (s.to_string(), st.map(str::to_string));
    assert_eq!(kinds(&c, "usr_a"), [k("login", Some("join me")), k("status", Some("busy")), k("logout", None)]);
    e(&c, "friend-location", json!({"location":"private"}));
    assert_eq!(kinds(&c, "usr_a").last().unwrap().0, "resume"); // online取りこぼし -> resume
    e(&c, "friend-online", json!({"platform":"web"}));
    assert_eq!(kinds(&c, "usr_a").iter().filter(|k| k.0 == "login").count(), 1);

    // コマンド: 現実的なデータで表示を確認
    {
        let cn = c.stats.conn();
        let conn = lock(&cn);
        conn.execute("DELETE FROM activity", []).unwrap();
        conn.execute("UPDATE stats_meta SET v=? WHERE k='since'", [t(1, 0, 0).to_string()]).unwrap();
    }
    for dd in 1..=8 {
        c.stats.record("usr_a", "login", Some("join me"), Some(t(dd, 21, 0)));
        c.stats.record("usr_a", "status", Some("busy"), Some(t(dd, 22, 0)));
        c.stats.record("usr_a", "logout", None, Some(t(dd, 23, 30)));
    }
    c.stats.record("usr_b", "login", Some("active"), Some(t(2, 9, 0)));
    c.stats.record("usr_b", "logout", None, Some(t(2, 10, 0)));
    let r = c.cmd_stats(1, 2, None, None);
    assert!(r.ephemeral && r.content.unwrap().contains("所有者")); // 他人は拒否
    assert!(c.cmd_stats(OWNER, 999, None, None).ephemeral); // 無関係チャンネルは拒否
    let em = c.cmd_stats(OWNER, MAIN, None, None).embed.unwrap();
    let f = fields(&em);
    let desc = em.description.clone().unwrap();
    assert!(em.title.as_ref().unwrap().contains("全員") && desc.contains("集計期間") && desc.contains("ゲーム内ログイン") && desc.contains("9回"));
    let freq = &f["⏰ ログイン頻度 (時間帯別)"];
    assert!(freq.contains("21時台(8回)") && freq.contains("9時台(1回)"));
    let st = f.iter().find(|(k, _)| k.starts_with("🎭")).unwrap().1;
    assert!(st.contains("join me") && st.contains("busy") && st.contains("active"), "{st}");
    let pct: f64 = st.split("**").filter_map(|s| s.strip_suffix('%')).filter_map(|s| s.parse::<f64>().ok()).sum();
    assert!((pct - 100.0).abs() < 0.2, "{pct}"); // ステータス別の割合
    let fr = f.iter().find(|(k, _)| k.starts_with("👥")).unwrap().1;
    assert!(fr.contains("Alice") && fr.contains("最多 21時台") && fr.contains("Bob\\*")); // 名前はエスケープ

    let em = c.cmd_stats(OWNER, 111, Some(3), None).embed.unwrap(); // フレンドチャンネル+days
    let exp = (1..=8).filter(|d| t(*d, 21, 0) >= vrc_friend_bot_rs::db::now() - 3.0 * 86400.0).count();
    let desc = em.description.clone().unwrap();
    assert!(em.title.as_ref().unwrap().contains("Alice") && !em.fields.iter().any(|x| x.name.starts_with("👥")) && desc.contains(&format!("{exp}回")) && desc.contains("約3.0日間"), "{desc}");
    c.db.deactivate("usr_b").unwrap();
    assert!(c.cmd_stats(OWNER, 222, None, None).embed.unwrap().title.unwrap().contains("削除済み")); // 削除済みも履歴を見られる

    // 巨大データでもDiscord上限(フィールド1024/全体6000)に収まる
    for n in 0..40 {
        let u = format!("usr_{n:02}");
        c.db.upsert(&u, &"名前".repeat(12), 5000 + n).unwrap();
        for h in 0..24 {
            c.stats.record(&u, "login", Some("active"), Some(t(3, h, 0)));
            c.stats.record(&u, "logout", None, Some(t(3, h, 30)));
        }
    }
    let em = c.cmd_stats(OWNER, MAIN, None, None).embed.unwrap();
    assert!(em.fields.iter().all(|x| x.value.chars().count() <= 1024), "{:?}", em.fields.iter().map(|x| x.value.chars().count()).collect::<Vec<_>>());
    assert!(em.len() <= 6000 && em.description.as_ref().unwrap().chars().count() <= 4096, "{}", em.len());
    assert!(em.fields.iter().find(|x| x.name.starts_with("👥")).unwrap().value.contains("…他"));

    // 削除(通知を止める)してもデータ収集は続く: ゲーム内状態は閉じない
    c.stats.record("usr_a", "login", Some("active"), None);
    c.state().ingame.insert("usr_a".into());
    assert!(c.view_check(OWNER).is_none() && c.view_check(1).unwrap().ephemeral);
    let r = c.confirm_yes(OWNER, "usr_a");
    assert!(r.content.unwrap().contains("削除しました") && r.embed.is_some());
    assert!(!c.db.get("usr_a").unwrap().unwrap().active && c.stats.state("usr_a").unwrap().0 && c.state().ingame.contains("usr_a"));
    assert!(c.confirm_yes(OWNER, "usr_a").content.unwrap().contains("すでに削除")); // 二度押し
    assert!(d.last().embed.title.unwrap().contains("メンバー削除")); // メンバーチャンネルにも履歴を残す

    // 起動時の最初の同期: 停止中にゲームを抜けていた人は、前回の生存時刻で閉じる
    c.db.upsert("usr_a", "Alice", 111).unwrap();
    lock(&c.stats.conn()).execute("DELETE FROM activity WHERE uid='usr_a'", []).unwrap();
    c.stats.record("usr_a", "login", Some("busy"), Some(t(1, 20, 0)));
    c.stats.prev_alive = Some(t(1, 21, 0));
    c.state().ingame.insert("usr_a".into());
    *v.friends.lock().unwrap() = vec![entry("usr_a", "Alice", false, "active", None)];
    c.sync_friends();
    let (logged, _, last_ts) = c.stats.state("usr_a").unwrap();
    assert!(!logged && last_ts == Some(t(1, 21, 0)) && !c.state().ingame.contains("usr_a"));
}

// ---------- /lang ----------

#[test]
fn lang_command_and_english_display() {
    let _g = serial();
    let (c, _, d) = newbot();
    c.db.upsert("usr_a", "Alice", 111).unwrap();
    let r = c.cmd_lang(1, "en");
    assert!(r.ephemeral && i18n::get_lang() == "ja"); // 他人は変更不可
    let r = c.cmd_lang(1, "fr");
    assert!(r.ephemeral && r.content.unwrap().contains("所有者")); // 他人には対応コードも教えない
    assert_eq!(c.cmd_lang(OWNER, "en").content.unwrap(), "Language switched to English.");
    assert_eq!(i18n::get_lang(), "en");
    let saved: String = lock(&c.db.c).query_row("SELECT v FROM bot_settings WHERE k='lang'", [], |r| r.get(0)).unwrap();
    assert_eq!(saved, "en"); // DBに保存
    let r = c.cmd_lang(OWNER, "fr");
    assert!(r.ephemeral && r.content.unwrap() == "Unsupported language. Available codes: ja, en");
    assert_eq!(i18n::get_lang(), "en"); // 失敗しても言語は変わらない
    assert_eq!(c.cmd_lang(OWNER, "EN-us").content.unwrap(), "Language switched to English."); // 大小文字・地域つきでも可

    // 通知(オンライン/移動/ステータス/オフライン)が英語になる
    let p = format!("{W}:1~group(grp_y)~groupAccessType(plus)~region(jp)");
    ev(&c, "friend-online", "usr_a", json!({"platform":"standalonewindows","location":p,"user":{"status":"join me"}}));
    let e = d.last().embed;
    assert_eq!(e.title.as_deref(), Some("🟢 Alice is online"));
    let f = fields(&e);
    for k in ["Platform", "World", "Instance", "Players", "Owner"] {
        assert!(f.contains_key(k), "{k}: {f:?}");
    }
    assert_eq!(f["Owner"], "MyGroup (Group)");
    ev(&c, "friend-location", "usr_a", json!({"location": format!("{W}:2~region(jp)")}));
    assert_eq!(d.last().embed.title.as_deref(), Some("🚪 Alice moved"));
    ev(&c, "friend-update", "usr_a", json!({"user":{"status":"busy","statusDescription":"x"}}));
    let e = d.last().embed;
    assert!(e.title.unwrap().ends_with("Alice changed status"));
    let names: Vec<&str> = e.fields.iter().map(|x| x.name.as_str()).collect();
    assert!(names == ["Before", "After"] || names == ["After"], "{names:?}"); // 直前のステータスが分からなければ After だけ (Python版のテストと同じ条件)
    ev(&c, "friend-offline", "usr_a", json!({}));
    assert_eq!(d.last().embed.title.as_deref(), Some("⚫ Alice went offline"));
    assert!(explain_error(&VrcError::Status(404)).contains("does not exist"));

    // 統計(コマンド)も英語
    lock(&c.stats.conn()).execute("UPDATE stats_meta SET v=? WHERE k='since'", [t(1, 0, 0).to_string()]).unwrap();
    c.stats.record("usr_a", "login", Some("busy"), Some(t(2, 21, 0)));
    c.stats.record("usr_a", "logout", None, Some(t(2, 22, 0)));
    let e = c.cmd_stats(OWNER, MAIN, None, None).embed.unwrap();
    let desc = e.description.clone().unwrap();
    assert!(e.title.as_ref().unwrap().contains("Login stats") && desc.contains("Period") && desc.contains("In-game logins"), "{desc}");
    assert!(!is_jp(e.title.as_ref().unwrap()) && !is_jp(&desc));
    for x in &e.fields {
        assert!(!is_jp(&x.name) && !is_jp(&x.value), "{x:?}");
    }
    let vals = format!("{:?}", e.fields.iter().map(|x| &x.value).collect::<Vec<_>>());
    assert!(!vals.contains("Unknown") && vals.contains("busy"));
    assert!(c.cmd_list(OWNER, MAIN, false, false).embed.unwrap().title.unwrap().contains("registered friends"));
    assert!(c.cmd_remove(OWNER, MAIN, "alice").embed.unwrap().title.unwrap().contains("Remove"));

    // jp は未対応(ISO 639ではない) / jpn で日本語に戻る
    let r = c.cmd_lang(OWNER, "jp");
    assert!(r.ephemeral && i18n::get_lang() == "en");
    assert!(c.cmd_lang(OWNER, "jpn").content.unwrap().contains("切り替えました"));
    assert_eq!(i18n::get_lang(), "ja");
    let saved: String = lock(&c.db.c).query_row("SELECT v FROM bot_settings WHERE k='lang'", [], |r| r.get(0)).unwrap();
    assert_eq!(saved, "ja"); // 保存されるのは正規コード
    ev(&c, "friend-online", "usr_a", json!({"platform":"android","location":"private"}));
    assert_eq!(d.last().embed.title.as_deref(), Some("🟢 Alice がオンライン"));
}

// ---------- /mention ----------

#[test]
fn mention_global_and_per_friend() {
    let _g = serial();
    let (c, _, d) = newbot();
    c.db.upsert("usr_a", "Alice", 111).unwrap();
    c.db.upsert("usr_b", "Bob*", 222).unwrap();
    c.db.upsert("usr_c", "Gone", 333).unwrap();
    c.db.deactivate("usr_c").unwrap();
    let m = format!("<@{OWNER}>");
    // ユーザーのオンライン通知で付いたメンションを返す (その後オフラインにする)
    let online = |u: &str| -> Option<String> {
        ev(&c, "friend-online", u, json!({"platform":"android","location":"private","user":{"status":"active"}}));
        let s = d.last();
        ev(&c, "friend-offline", u, json!({}));
        s.content
    };
    assert!(c.state().mention_on); // 既定はオン(従来動作)
    assert_eq!(online("usr_a"), Some(m.clone()));
    assert_eq!(d.last().content, None); // オンライン以外(offline通知)はメンションしない

    assert!(c.cmd_mention(1, MAIN, "off").ephemeral && c.state().mention_on); // 他人は変更不可
    assert!(c.cmd_mention(OWNER, MAIN, "x").ephemeral && c.state().mention_on); // 不正な値は無視
    let r = c.cmd_mention(OWNER, MAIN, "off");
    assert!(!c.state().mention_on && r.content.as_ref().unwrap().starts_with("🔕") && r.content.unwrap().contains("オフ"));
    assert_eq!(c.get_setting("mention").as_deref(), Some("0")); // DBに保存
    let n = d.n();
    assert_eq!(online("usr_a"), None); // オフでも通知自体は送る(メンションだけ外す)
    assert!(d.sent.lock().unwrap()[n].embed.title.as_ref().unwrap().starts_with("🟢"));
    let desc = c.cmd_list(OWNER, MAIN, false, false).embed.unwrap().description.unwrap();
    assert!(desc.starts_with("🔕 オンライン通知のメンション(全体設定): オフ") && desc.contains("Alice"));
    let r = c.cmd_mention(OWNER, MAIN, "on");
    assert!(c.state().mention_on && r.content.unwrap().starts_with("🔔"));
    assert_eq!(online("usr_a"), Some(m.clone()));

    // 英語
    i18n::set_lang("en").unwrap();
    assert_eq!(c.cmd_mention(OWNER, MAIN, "off").content.unwrap(), "🔕 Online mention (global) turned OFF.");
    assert_eq!(c.cmd_mention(OWNER, MAIN, "x").content.unwrap(), "Please specify on, off or reset.");
    assert!(c.cmd_list(OWNER, MAIN, false, false).embed.unwrap().description.unwrap().starts_with("🔕 Online mention (global): OFF"));
    assert_eq!(c.cmd_mention(OWNER, MAIN, "on").content.unwrap(), "🔔 Online mention (global) turned ON.");
    i18n::set_lang("ja").unwrap();

    // ---- フレンド別 ----
    let both = || (online("usr_a"), online("usr_b"));
    assert_eq!(both(), (Some(m.clone()), Some(m.clone()))); // 既定: 全員メンション
    let r = c.cmd_mention(OWNER, 111, "off"); // Aliceのチャンネルでoff -> Aliceだけ
    assert!(r.content.as_ref().unwrap().starts_with("🔕") && r.content.unwrap().contains("Alice") && c.state().mention_on);
    assert_eq!(c.get_setting("mention:usr_a").as_deref(), Some("0"));
    assert_eq!(both(), (None, Some(m.clone())));
    assert!(c.cmd_mention(OWNER, MAIN, "off").content.unwrap().contains("全体設定")); // メインでoff -> 全体(個別設定のないBob)
    assert_eq!(both(), (None, None));
    assert!(c.cmd_mention(OWNER, 111, "on").content.unwrap().starts_with("🔔")); // 個別onは全体offより優先
    assert_eq!(both(), (Some(m.clone()), None));
    let r = c.cmd_mention(OWNER, 111, "reset"); // reset -> 全体設定(off)に従う
    let txt = r.content.unwrap();
    assert!(txt.contains("全体設定に戻しました") && txt.contains("オフ") && c.get_setting("mention:usr_a").is_none());
    assert_eq!(online("usr_a"), None);
    c.cmd_mention(OWNER, MAIN, "on");
    assert_eq!(online("usr_a"), Some(m.clone())); // 全体onに戻せば個別なしのAliceも追従
    let r = c.cmd_mention(OWNER, MAIN, "reset");
    assert!(r.ephemeral && r.content.unwrap().contains("フレンド用チャンネル")); // メインでresetは不可
    let r = c.cmd_mention(OWNER, 999, "off");
    assert!(r.ephemeral && r.content.unwrap().contains("メインチャンネルか")); // 無関係チャンネルは不可
    assert!(c.cmd_mention(1, 111, "off").ephemeral && c.get_setting("mention:usr_a").is_none()); // 他人は不可
    assert!(c.cmd_mention(OWNER, 333, "off").content.unwrap().contains("Gone")); // 削除済みフレンドのチャンネルでも設定可
    assert!(c.cmd_mention(OWNER, 222, "off").content.unwrap().contains("Bob\\*")); // 名前はエスケープ

    // 一覧: 左の印=メンション状態、個別設定には(個別)
    c.del_setting("mention:usr_c");
    c.state().mention_on = true;
    c.cmd_mention(OWNER, 111, "off"); // Alice=個別off
    c.cmd_mention(OWNER, 222, "reset"); // Bobを全体(on)に戻す
    let d2 = c.cmd_list(OWNER, MAIN, false, true).embed.unwrap().description.unwrap(); // 削除済みも表示
    let row = |n: &str| d2.split('\n').find(|l| l.contains(n)).unwrap().to_string();
    assert!(row("Alice").starts_with("🔕") && row("Alice").contains("(個別)"));
    assert!(row("Bob").starts_with("🔔") && !row("Bob").contains("(個別)"));
    assert!(row("Gone").starts_with("🚫") && !d2.contains('✅') && d2.contains("(個別)=フレンド別の設定"));
    i18n::set_lang("en").unwrap();
    assert_eq!(c.cmd_mention(OWNER, 111, "on").content.unwrap(), "🔔 Online mention for Alice turned ON.");
    assert_eq!(c.cmd_mention(OWNER, 111, "reset").content.unwrap(), "↩️ Alice now follows the global setting (ON).");
    c.cmd_mention(OWNER, 111, "off");
    let en = c.cmd_list(OWNER, MAIN, false, false).embed.unwrap().description.unwrap();
    assert!(en.contains("(custom)") && en.contains("per-friend setting"));
    assert_eq!(c.cmd_mention(OWNER, MAIN, "reset").content.unwrap(), "Use reset in a friend's channel.");
}

#[test]
fn settings_survive_restart() {
    let _g = serial();
    let dir = std::env::temp_dir().join(format!("vrcrs-bot-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("restart.db");
    let _ = std::fs::remove_file(&path);
    let mk = || {
        let db = Db::open(path.to_str().unwrap()).unwrap();
        let stats = Stats::new(db.c.clone(), Tokyo).unwrap();
        Core::new(db, stats, Arc::new(FakeVrc::default()), Arc::new(FakeDiscord::default()), Config { main_ch: MAIN, owner: OWNER })
    };
    let b1 = mk();
    assert!(b1.state().mention_on);
    b1.set_setting("mention", "0");
    let b2 = mk();
    assert!(!b2.state().mention_on);
    b2.set_setting("mention", "1");
    assert!(mk().state().mention_on);
    b2.set_setting("mention:usr_z", "0");
    let b3 = mk();
    assert!(!b3.mention_for("usr_z") && b3.mention_overrides() == HashMap::from([("usr_z".to_string(), false)]));
    b3.del_setting("mention:usr_z");
    assert!(mk().mention_overrides().is_empty() && mk().mention_for("usr_z"));
}

// ---------- コマンドの権限 ----------

#[test]
fn commands_are_guarded() {
    let _g = serial();
    let (c, _, _) = newbot();
    c.db.upsert("usr_a", "Alice", 111).unwrap();
    for (who, ch) in [(OWNER, 999), (1, MAIN)] {
        let rs = [
            c.cmd_add_prepare(who, ch, "usr_x").err().unwrap(),
            c.cmd_remove(who, ch, "usr_a"),
            c.cmd_list(who, ch, false, false),
        ];
        for r in rs {
            assert!(r.ephemeral && r.content.unwrap().contains("メインチャンネルで所有者のみ"), "{who} {ch}");
        }
    }
    assert!(c.cmd_list(OWNER, MAIN, false, false).embed.unwrap().description.unwrap().contains("Alice")); // 正規の場所なら動く
    let r = c.cmd_add_prepare(OWNER, MAIN, "not-a-url").err().unwrap();
    assert!(r.content.unwrap().contains("該当するフレンドが見つかりません"));
    // removeのオートコンプリート: 所有者のみ・値はUID
    let r = c.ac_remove(OWNER, "ali");
    assert_eq!(r.iter().map(|x| (x.name.split(' ').next().unwrap().to_string(), x.value.clone())).collect::<Vec<_>>(), [("Alice".to_string(), "usr_a".to_string())]);
    assert!(c.ac_remove(1, "ali").is_empty());
    // 確認ボタンの付いた返答 / 登録なしの一覧
    let r = c.cmd_remove(OWNER, MAIN, "alice");
    assert!(matches!(r.view, Some(View::Confirm { ref uid, .. }) if uid == "usr_a"));
    assert!(c.cmd_remove(OWNER, MAIN, "nobody").content.unwrap().contains("存在しません"));
    assert_eq!(c.confirm_no("Bob*").content.unwrap(), "キャンセルしました: Bob\\*");
    let (c2, _, _) = newbot();
    assert_eq!(c2.cmd_list(OWNER, MAIN, false, false).content.as_deref(), Some("登録なし"));
}

// ---------- 全フレンドの収集 ----------

#[test]
fn sync_and_events_record_everyone_but_notify_registered_only() {
    let _g = serial();
    let (c, v, d) = newbot();
    c.db.upsert("usr_a", "OldA", 111).unwrap(); // 登録済み(名前が古い)
    *v.friends.lock().unwrap() = vec![
        entry("usr_a", "Alice", true, "join me", Some("wrld_x:1~region(jp)")),
        entry("usr_b", "Bob", true, "busy", Some("private")),
        entry("usr_c", "Carol", false, "active", None),
        entry("usr_t", "Trav", true, "active", Some("traveling")),
    ];
    c.sync_friends();
    let roster: std::collections::BTreeSet<String> = c.stats.friends().unwrap().into_iter().map(|r| r.uid).collect();
    assert_eq!(roster, ["usr_a", "usr_b", "usr_c", "usr_t"].map(String::from).into_iter().collect());
    assert_eq!(c.db.get("usr_a").unwrap().unwrap().name.as_deref(), Some("Alice")); // 登録済みの表示名も同期
    {
        let st = c.state();
        assert_eq!(st.ingame, ["usr_a", "usr_b", "usr_t"].map(String::from).into_iter().collect());
        assert_eq!(st.last_status["usr_b"], ("busy".to_string(), String::new()));
        assert_eq!(st.last_loc["usr_a"], "wrld_x:1~region(jp)");
        assert!(!st.last_loc.contains_key("usr_t"));
        assert!(!st.first_sync && st.ws_down_at.is_none());
    }
    assert_eq!(kinds(&c, "usr_a"), [("resume".to_string(), Some("join me".to_string()))]);
    assert!(kinds(&c, "usr_c").is_empty());
    assert!(d.n() == 0 && v.world_calls.load(Ordering::SeqCst) == 0 && v.instance_calls.load(Ordering::SeqCst) == 0 && v.owner_calls.load(Ordering::SeqCst) == 0); // 同期は通知もワールドAPIもなし

    // 未登録のBob: 統計は記録するが、通知もワールドAPIもなし
    ev(&c, "friend-offline", "usr_b", json!({}));
    ev(&c, "friend-online", "usr_b", json!({"platform":"android","location":"wrld_x:2","user":{"status":"join me"}}));
    ev(&c, "friend-update", "usr_b", json!({"user":{"status":"ask me","statusDescription":""}}));
    ev(&c, "friend-location", "usr_b", json!({"location":"wrld_y:3"}));
    let k = |s: &str, st: Option<&str>| (s.to_string(), st.map(str::to_string));
    assert_eq!(kinds(&c, "usr_b"), [k("resume", Some("busy")), k("logout", None), k("login", Some("join me")), k("status", Some("ask me"))]);
    assert!(d.n() == 0 && v.world_calls.load(Ordering::SeqCst) == 0);

    // 登録済みのAliceは従来どおり通知(オンラインはメンション付き)
    ev(&c, "friend-offline", "usr_a", json!({}));
    assert_eq!(d.last().embed.title.as_deref(), Some("⚫ Alice がオフライン"));
    ev(&c, "friend-online", "usr_a", json!({"platform":"android","location":"private","user":{"status":"active"}}));
    let s = d.last();
    assert!(s.embed.title.as_deref() == Some("🟢 Alice がオンライン") && s.content == Some(format!("<@{OWNER}>")));

    // 削除(通知停止)後も記録は続き、通知は出ない
    c.db.deactivate("usr_a").unwrap();
    let n = d.n();
    ev(&c, "friend-offline", "usr_a", json!({}));
    ev(&c, "friend-online", "usr_a", json!({"platform":"android","location":"private","user":{"status":"busy"}}));
    assert_eq!(d.n(), n);
    assert_eq!(kinds(&c, "usr_a")[kinds(&c, "usr_a").len() - 2..], [k("logout", None), k("login", Some("busy"))]);

    // フレンド追加/解除/改名
    ev(&c, "friend-add", "usr_e", json!({"user":{"displayName":"Eve"}}));
    let e = c.stats.friend("usr_e").unwrap().unwrap();
    assert!(e.name.as_deref() == Some("Eve") && e.is_friend);
    ev(&c, "friend-online", "usr_e", json!({"platform":"windows","location":"private","user":{"displayName":"Eve","status":"active"}}));
    assert!(c.state().ingame.contains("usr_e"));
    ev(&c, "friend-delete", "usr_e", json!({}));
    assert!(!c.stats.friend("usr_e").unwrap().unwrap().is_friend && !c.state().ingame.contains("usr_e") && kinds(&c, "usr_e").last().unwrap().0 == "logout");
    ev(&c, "friend-update", "usr_b", json!({"user":{"displayName":"Bobby","status":"ask me","statusDescription":""}}));
    assert_eq!(c.stats.friend("usr_b").unwrap().unwrap().name.as_deref(), Some("Bobby"));
    ev(&c, "friend-update", "usr_a", json!({"user":{"displayName":"Alicia","status":"busy"}}));
    assert_eq!(c.db.get("usr_a").unwrap().unwrap().name.as_deref(), Some("Alicia")); // 登録側の表示名にも反映
    ev(&c, "friend-add", "usr_f", json!({"user":{}}));
    assert_eq!(c.stats.friend("usr_f").unwrap().unwrap().name, None); // 名前不明でも名簿に載る
}

#[test]
fn resync_closes_gaps_and_has_safety_valve() {
    let _g = serial();
    let (mut c, v, _) = newbot();
    for n in 0..10 {
        c.stats.upsert_friend(&uu(n), Some(&format!("F{n}")), None).unwrap();
    }
    c.stats.record(&uu(1), "login", Some("active"), Some(t(1, 20, 0)));
    c.state().ingame.insert(uu(1));
    c.stats.record(&uu(2), "login", Some("busy"), Some(t(1, 20, 0)));
    c.state().ingame.insert(uu(2));
    let base: Vec<FriendEntry> = (0..10).map(|n| entry(&uu(n), &format!("F{n}"), false, "active", None)).collect();
    // 同期が失敗しても first_sync / ws_down_at を持ち越し、成功した最初の同期は前回の生存時刻で閉じる
    c.stats.prev_alive = Some(t(1, 21, 0));
    {
        let mut st = c.state();
        st.first_sync = true;
        st.ws_down_at = Some(t(1, 23, 0));
    }
    v.fail_friends.store(true, Ordering::SeqCst);
    c.sync_friends();
    assert!(c.state().first_sync && c.state().ws_down_at == Some(t(1, 23, 0)));
    v.fail_friends.store(false, Ordering::SeqCst);
    *v.friends.lock().unwrap() = base.clone();
    c.sync_friends();
    let s1 = c.stats.state(&uu(1)).unwrap();
    assert!(s1.2 == Some(t(1, 21, 0)) && !s1.0 && !c.state().first_sync && c.state().ws_down_at.is_none());
    assert_eq!(c.stats.state(&uu(2)).unwrap().2, Some(t(1, 21, 0))); // (1回目で閉じ済み)

    // 2回目以降は WebSocket が切れた時刻で閉じる
    c.stats.record(&uu(4), "login", Some("active"), Some(t(1, 20, 0)));
    c.state().ingame.insert(uu(4));
    c.state().ws_down_at = Some(t(1, 22, 0));
    c.sync_friends();
    let s4 = c.stats.state(&uu(4)).unwrap();
    assert!(!s4.0 && s4.2 == Some(t(1, 22, 0)) && c.state().ws_down_at.is_none());

    // 一覧から消えた人(少数)はフレンド解除として扱い、ゲーム内なら閉じる
    c.stats.record(&uu(3), "login", Some("active"), Some(t(1, 20, 0)));
    c.state().ingame.insert(uu(3));
    *v.friends.lock().unwrap() = base.iter().filter(|f| f.uid != uu(3)).cloned().collect();
    c.sync_friends();
    assert!(!c.stats.friend(&uu(3)).unwrap().unwrap().is_friend && !c.state().ingame.contains(&uu(3)) && !c.stats.state(&uu(3)).unwrap().0);

    // 取得不全で大量に消えたように見えたら解除扱いにしない(安全弁)
    for n in 10..30 {
        c.stats.upsert_friend(&uu(n), Some(&format!("F{n}")), None).unwrap();
    }
    *v.friends.lock().unwrap() = base[..10].to_vec();
    c.sync_friends();
    assert!((10..30).all(|n| c.stats.friend(&uu(n)).unwrap().unwrap().is_friend));
    let _ = &mut c;
}

#[test]
fn concurrent_sync_is_ignored() {
    let _g = serial();
    let (c, v, _) = newbot();
    let c = Arc::new(c);
    let gate = v.gate.lock().unwrap(); // 取得を止めておく
    let c2 = c.clone();
    let h = std::thread::spawn(move || c2.sync_friends());
    while !v.in_call.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    c.sync_friends(); // 同期中の二重実行は無視される(取得を呼ばない)
    assert_eq!(v.friend_calls.load(Ordering::SeqCst), 1);
    drop(gate);
    h.join().unwrap();
    c.sync_friends(); // 終われば再び実行できる
    assert_eq!(v.friend_calls.load(Ordering::SeqCst), 2);
}

// ---------- /add ----------

#[test]
fn add_from_roster_or_url() {
    let _g = serial();
    let (c, v, d) = newbot();
    for (n, name) in [(1, "Alice"), (2, "Bob"), (3, "Zed"), (4, "Gone"), (5, "Reg")] {
        c.stats.upsert_friend(&uu(n), Some(name), None).unwrap();
    }
    c.stats.mark_unfriended(&uu(4)).unwrap();
    c.db.upsert(&uu(5), "Reg", 555).unwrap();
    let cand: std::collections::BTreeSet<String> = c.add_candidates().into_iter().map(|r| r.uid).collect();
    assert_eq!(cand, [uu(1), uu(2), uu(3)].into_iter().collect()); // 解除済み・登録済みは候補に出ない
    let r = c.ac_add(OWNER, "ali");
    assert_eq!(r.iter().map(|x| (x.name.split(' ').next().unwrap().to_string(), x.value.clone())).collect::<Vec<_>>(), [("Alice".to_string(), uu(1))]);
    assert!(c.ac_add(OWNER, "").len() == 3 && c.ac_add(1, "").is_empty());

    let r = do_add(&c, OWNER, MAIN, &uu(1)); // 名簿の人はAPI不要
    assert!(v.user_calls.load(Ordering::SeqCst) == 0 && d.created.lock().unwrap().as_slice() == [(uu(1), "alice".to_string())] && c.db.get(&uu(1)).unwrap().unwrap().active);
    let txt = r.content.unwrap();
    assert!(txt.contains("追加しました") && txt.contains("Alice") && !txt.contains("⚠️") && txt.contains("<#901>"), "{txt}");
    assert!(!c.add_candidates().iter().any(|r| r.uid == uu(1)));
    assert!(do_add(&c, OWNER, MAIN, &uu(1)).content.unwrap().contains("すでに登録済み"));
    do_add(&c, OWNER, MAIN, " bob "); // 名前の完全一致(一意)
    assert!(c.db.get(&uu(2)).unwrap().is_some() && v.user_calls.load(Ordering::SeqCst) == 0);
    c.stats.upsert_friend(&uu(8), Some("Dup"), None).unwrap();
    c.stats.upsert_friend(&uu(9), Some("dup"), None).unwrap();
    assert!(do_add(&c, OWNER, MAIN, "DUP").content.unwrap().contains("同名") && c.db.get(&uu(8)).unwrap().is_none());
    assert!(do_add(&c, OWNER, MAIN, "nobody").content.unwrap().contains("該当するフレンドが見つかりません"));
    do_add(&c, OWNER, MAIN, &format!("https://vrchat.com/home/user/{}", uu(3))); // URLでも名簿の人はAPI不要
    assert!(c.db.get(&uu(3)).unwrap().is_some() && v.user_calls.load(Ordering::SeqCst) == 0);

    // 名簿にいない人(URL): APIで確認 -> フレンドなら名簿に載せる / フレンドでなければ警告 / 存在しなければエラー
    let info = |name: &str, friend: bool| UserInfo { name: name.into(), status: "active".into(), desc: String::new(), is_friend: friend, state: "offline".into() };
    {
        let mut u = v.users.lock().unwrap();
        u.insert(uu(20), Ok(info("Zoe", true)));
        u.insert(uu(21), Ok(info("Stranger", false)));
        u.insert(uu(22), Err(404));
    }
    let r = do_add(&c, OWNER, MAIN, &format!("https://vrchat.com/home/user/{}", uu(20)));
    assert!(v.user_calls.load(Ordering::SeqCst) == 1 && c.db.get(&uu(20)).unwrap().unwrap().name.as_deref() == Some("Zoe"));
    assert!(c.stats.friend(&uu(20)).unwrap().unwrap().is_friend && !r.content.unwrap().contains("⚠️"));
    let r = do_add(&c, OWNER, MAIN, &uu(21));
    assert!(r.content.unwrap().contains("⚠️") && c.stats.friend(&uu(21)).unwrap().is_none() && c.db.get(&uu(21)).unwrap().unwrap().active);
    let r = do_add(&c, OWNER, MAIN, &uu(22));
    assert!(r.content.unwrap().contains("存在しません") && c.db.get(&uu(22)).unwrap().is_none());

    // 削除した人は再登録できる(候補に戻り、チャンネルは再利用)
    c.db.deactivate(&uu(1)).unwrap();
    assert!(c.add_candidates().iter().any(|r| r.uid == uu(1)));
    let n_created = d.created.lock().unwrap().len();
    let r = do_add(&c, OWNER, MAIN, &uu(1));
    assert!(r.embed.unwrap().title.unwrap().contains("(再登録)"));
    assert_eq!(d.created.lock().unwrap().len(), n_created); // 新しいチャンネルは作らない
    assert!(d.sent.lock().unwrap().iter().any(|s| s.ch == 901 && s.embed.title.as_deref().map_or(false, |t| t.contains("再登録")))); // メンバーチャンネルに履歴
}

// ---------- /stats who ----------

#[test]
fn stats_who_works_without_registration() {
    let _g = serial();
    let (c, _, _) = newbot();
    let (r_, a_, b_, u_, l_) = (uu(1), uu(2), uu(3), uu(4), uu(5)); // 削除済み/登録中/未登録/解除済み/名簿に無い登録済み
    c.db.upsert(&a_, "Alice", 111).unwrap();
    c.db.upsert(&r_, "Removed", 222).unwrap();
    c.db.deactivate(&r_).unwrap();
    c.db.upsert(&l_, "Legacy", 555).unwrap();
    for (u, n) in [(&a_, "Alice"), (&r_, "Removed"), (&b_, "Bob"), (&u_, "Unfr")] {
        c.stats.upsert_friend(u, Some(n), None).unwrap();
    }
    c.stats.mark_unfriended(&u_).unwrap();
    lock(&c.stats.conn()).execute("UPDATE vrc_friends SET tracked_since=?", [t(4, 0, 0)]).unwrap(); // 収集開始は全員4日目(履歴はその後)
    for (u, k) in [(&a_, 2), (&r_, 3), (&b_, 5), (&u_, 7)] {
        for dd in 0..k {
            c.stats.record(u, "login", Some("active"), Some(t(5 + dd / 4, 8 + 3 * (dd % 4), 0)));
            c.stats.record(u, "logout", None, Some(t(5 + dd / 4, 9 + 3 * (dd % 4), 0)));
        }
    }
    let em = c.cmd_stats(OWNER, MAIN, None, None).embed.unwrap();
    assert!(em.description.unwrap().contains("**ゲーム内ログイン**: 2回") && em.title.unwrap().contains("全員")); // 省略時は登録中の全員
    for (who, n, tags) in [(&b_, 5, vec![" (未登録)"]), (&r_, 3, vec![" (削除済み)"]), (&u_, 7, vec![" (未登録)", " (フレンド解除済み)"]), (&a_, 2, vec![])] {
        let em = c.cmd_stats(OWNER, MAIN, None, Some(who)).embed.unwrap();
        let (title, desc) = (em.title.clone().unwrap(), em.description.clone().unwrap());
        assert!(desc.contains(&format!("**ゲーム内ログイン**: {n}回")) && tags.iter().all(|t| title.contains(t)), "{who} {title} {desc}");
        assert!(!em.fields.iter().any(|x| x.name.starts_with("👥")));
    }
    let fmt = |ts: f64| Tokyo.timestamp_opt(ts as i64, 0).unwrap().format("%Y-%m-%d %H:%M").to_string();
    assert!(c.cmd_stats(OWNER, MAIN, None, Some(&b_)).embed.unwrap().description.unwrap().contains(&fmt(t(4, 0, 0)))); // 期間の開始はそのフレンドの収集開始日時
    let d = c.cmd_stats(OWNER, MAIN, Some(1), Some(&b_)).embed.unwrap().description.unwrap(); // days併用
    assert!(d.contains("約1.0日間") && d.contains("**ゲーム内ログイン**: 0回"));
    assert!(c.cmd_stats(OWNER, MAIN, None, Some("bob")).embed.unwrap().title.unwrap().contains("Bob")); // 名前でも指定できる
    assert!(c.cmd_stats(OWNER, 111, None, Some(&b_)).embed.unwrap().title.unwrap().contains("Bob")); // フレンド用チャンネルでもwho優先
    assert!(c.cmd_stats(OWNER, 111, None, None).embed.unwrap().title.unwrap().contains("Alice")); // whoなしは従来どおりそのチャンネルのフレンド
    assert!(c.cmd_stats(OWNER, 222, None, None).embed.unwrap().title.unwrap().contains("(削除済み)"));
    assert!(c.cmd_stats(OWNER, MAIN, None, Some(&l_)).embed.unwrap().title.unwrap().contains("Legacy")); // 名簿に無い登録済みの人も指定できる
    let r = c.cmd_stats(OWNER, MAIN, None, Some("nobody"));
    assert!(r.ephemeral && r.content.unwrap().contains("該当するフレンドが見つかりません"));
    assert!(c.cmd_stats(OWNER, MAIN, None, Some(&uu(99))).ephemeral);
    c.stats.upsert_friend(&uu(8), Some("Same"), None).unwrap();
    c.stats.upsert_friend(&uu(9), Some("same"), None).unwrap();
    assert!(c.cmd_stats(OWNER, MAIN, None, Some("SAME")).content.unwrap().contains("同名"));
    let r = c.cmd_stats(1, MAIN, None, Some(&b_));
    assert!(r.ephemeral && r.content.unwrap().contains("所有者")); // 他人は不可
    let r = c.ac_who(OWNER, ""); // 候補: 登録中/削除済み/未登録/解除済み/名簿外の登録済み
    let vals: std::collections::HashSet<_> = r.iter().map(|x| x.value.clone()).collect();
    assert!([&a_, &r_, &b_, &u_, &l_].iter().all(|u| vals.contains(*u)) && c.ac_who(1, "").is_empty());
    let r = c.ac_who(OWNER, "unf");
    assert_eq!(r.iter().map(|x| (x.name.split(' ').next().unwrap().to_string(), x.value.clone())).collect::<Vec<_>>(), [("Unfr".to_string(), u_.clone())]);
}

// ---------- 記録は他のBot(Python版)に任せ、通知・集計・/add だけ動かす ----------

#[test]
fn readonly_stats_mode_notifies_but_never_writes_to_the_shared_db() {
    let _g = serial();
    // 記録担当(Python版に相当)が作った統計DB
    let path = std::env::temp_dir().join(format!("vrcrs-ro-core-{}.db", std::process::id())).to_string_lossy().to_string();
    let _ = std::fs::remove_file(&path);
    {
        let w = Stats::new(vrc_friend_bot_rs::db::open_conn(&path, false).unwrap(), Tokyo).unwrap();
        lock(&w.conn()).execute("UPDATE stats_meta SET v=? WHERE k='since'", [t(1, 0, 0).to_string()]).unwrap();
        for (u, n) in [("usr_a", "Alice"), ("usr_b", "Bob")] {
            w.upsert_friend(u, Some(n), None).unwrap();
        }
        lock(&w.conn()).execute("UPDATE vrc_friends SET tracked_since=?", [t(1, 0, 0)]).unwrap(); // 収集開始は過去
        w.record("usr_a", "login", Some("busy"), Some(t(2, 20, 0)));
        w.record("usr_a", "logout", None, Some(t(2, 21, 0)));
        w.heartbeat();
    }
    let snapshot = |p: &str| {
        let c = rusqlite::Connection::open(p).unwrap();
        let q = |sql: &str| c.query_row(sql, [], |r| r.get::<_, String>(0)).unwrap();
        (q("SELECT group_concat(uid||kind||ts) FROM activity"), q("SELECT group_concat(uid||name||is_friend||tracked_since) FROM vrc_friends"), q("SELECT group_concat(k||v) FROM stats_meta"))
    };
    let before = snapshot(&path);

    // Rust版: 登録(friends)は別DB、統計は上のDBを読み取り専用で参照
    let db = Db::memory().unwrap();
    let stats = Stats::new_readonly(vrc_friend_bot_rs::db::open_conn(&path, true).unwrap(), Tokyo).unwrap();
    let (v, d) = (Arc::new(FakeVrc::default()), Arc::new(FakeDiscord::default()));
    let c = Core::new(db, stats, v.clone(), d.clone(), Config { main_ch: MAIN, owner: OWNER });
    c.db.upsert("usr_a", "Alice", 111).unwrap();

    // 通知は従来どおり届く (ゲーム内の状態はメモリ上で追う)
    ev(&c, "friend-online", "usr_a", json!({"platform":"android","location":"private","user":{"status":"active","displayName":"Alice"}}));
    ev(&c, "friend-update", "usr_a", json!({"user":{"status":"busy"}}));
    ev(&c, "friend-offline", "usr_a", json!({}));
    assert_eq!(d.titles()[0], "🟢 Alice がオンライン");
    assert_eq!(d.titles().last().unwrap(), "⚫ Alice がオフライン");
    // 新しいフレンドの追加・解除・同期の補正も、共有DBには書かない
    ev(&c, "friend-add", "usr_n", json!({"user":{"displayName":"Newbie"}}));
    ev(&c, "friend-delete", "usr_b", json!({}));
    *v.friends.lock().unwrap() = vec![entry("usr_a", "Alice", true, "active", None), entry("usr_x", "Xavier", false, "active", None)];
    c.sync_friends();
    c.stats.heartbeat();
    assert_eq!(snapshot(&path), before, "読み取り専用のあいだ、共有DBは1行も変わらない");

    // 集計と /add の候補は、共有DBの記録をそのまま使う
    let em = c.cmd_stats(OWNER, 111, None, None).embed.unwrap();
    assert!(em.title.unwrap().contains("Alice") && em.description.unwrap().contains("**ゲーム内ログイン**: 1回"));
    assert!(c.add_candidates().iter().any(|r| r.uid == "usr_b")); // 共有の名簿にいる人が候補に出る
}
