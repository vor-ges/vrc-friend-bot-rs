//! bot.py の `Bot` と `FriendCommands` の移植。Discord本体には触れず、`Discord` / `VrcApi` という窓口(trait)越しに動く。
//!
//! - 状態の更新は `state` のロックの中、通信(VRChat API / Discord送信)はロックの外で行う。
//!   Python版の「await の合間にしか割り込まれない」性質を、スレッドでも保つため。
//! - スラッシュコマンドは Discord に依存しない `Reply` を返す。ボタン付きは `View` で表す。
use crate::db::{now, Db, Friend};
use crate::i18n;
use crate::logic::*;
use crate::stats::{Stats, VrcFriend};
use crate::tr;
use crate::vrc::{explain_error, extract_uid, FriendEntry, InstanceInfo, UserInfo, Vrc, VrcError, WorldInfo};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

// ---------- 外部との窓口 ----------

/// VRChat API (本物は `Vrc`、テストでは偽物)。
pub trait VrcApi: Send + Sync {
    fn login(&self) -> Result<(), VrcError>;
    fn get_token(&self) -> Result<String, VrcError>;
    fn get_user(&self, uid: &str) -> Result<UserInfo, VrcError>;
    fn get_world(&self, wid: &str) -> Result<WorldInfo, VrcError>;
    fn get_instance(&self, wid: &str, iid: &str) -> Result<InstanceInfo, VrcError>;
    fn get_group_name(&self, gid: &str) -> Result<String, VrcError>;
    fn get_friends_all(&self) -> Result<Vec<FriendEntry>, VrcError>;
}

impl VrcApi for Vrc {
    fn login(&self) -> Result<(), VrcError> {
        Vrc::login(self)
    }
    fn get_token(&self) -> Result<String, VrcError> {
        Vrc::get_token(self)
    }
    fn get_user(&self, uid: &str) -> Result<UserInfo, VrcError> {
        Vrc::get_user(self, uid)
    }
    fn get_world(&self, wid: &str) -> Result<WorldInfo, VrcError> {
        Vrc::get_world(self, wid)
    }
    fn get_instance(&self, wid: &str, iid: &str) -> Result<InstanceInfo, VrcError> {
        Vrc::get_instance(self, wid, iid)
    }
    fn get_group_name(&self, gid: &str) -> Result<String, VrcError> {
        Vrc::get_group_name(self, gid)
    }
    fn get_friends_all(&self) -> Result<Vec<FriendEntry>, VrcError> {
        Vrc::get_friends_all(self)
    }
}

#[derive(Debug, Clone)]
pub struct DiscordError {
    /// HTTPステータス (Python版の discord.HTTPException.status)
    pub status: Option<u16>,
    pub message: String,
}

impl std::fmt::Display for DiscordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "discord error (status {:?}): {}", self.status, self.message)
    }
}
impl std::error::Error for DiscordError {}

/// Discordへの書き込み (本物は discord.rs、テストでは偽物)。
pub trait Discord: Send + Sync {
    /// チャンネルに送る。
    fn send(&self, channel_id: i64, content: Option<&str>, embed: &Embed) -> Result<(), DiscordError>;
    /// フレンド用チャンネルを返す。existing が実在すればそれを再利用し、無ければ「VRC-Friends」カテゴリ
    /// (無ければ作る)の下に `chan_name` で作って topic に uid を入れる。
    fn friend_channel(&self, existing: Option<i64>, chan_name: &str, uid: &str) -> Result<i64, DiscordError>;
    /// チャンネルが存在し、Botから見えるか (見えない・存在しないなら false)
    fn channel_exists(&self, channel_id: i64) -> Result<bool, DiscordError>;
}

// ---------- 返答 ----------

#[derive(Debug, Clone)]
pub enum View {
    /// 削除の確認ボタン (削除する / キャンセル)
    Confirm { uid: String, name: String },
    /// 一覧のページ送り (◀ ▶)
    List(ListView),
}

/// スラッシュコマンド/ボタンへの返答。embed が None のとき、ボタンからの更新では既存の埋め込みを消す。
#[derive(Debug, Clone, Default)]
pub struct Reply {
    pub content: Option<String>,
    pub embed: Option<Embed>,
    pub ephemeral: bool,
    pub view: Option<View>,
}

impl Reply {
    fn text(s: String) -> Self {
        Reply { content: Some(s), ..Default::default() }
    }
    fn private(s: String) -> Self {
        Reply { content: Some(s), ephemeral: true, ..Default::default() }
    }
    fn embed(e: Embed) -> Self {
        Reply { embed: Some(e), ..Default::default() }
    }
}

// ---------- 状態 ----------

#[derive(Debug, Clone)]
pub struct Config {
    /// コマンドを受け付けるメインチャンネル (MAIN_CHANNEL_ID)
    pub main_ch: i64,
    /// 所有者のDiscordユーザーID (OWNER_ID): メンション先・コマンド実行者
    pub owner: i64,
}

pub struct State {
    pub last_loc: HashMap<String, String>,
    pub last_status: HashMap<String, (String, String)>,
    world_cache: HashMap<String, (f64, Option<WorldInfo>)>,
    owner_cache: HashMap<String, (f64, Option<String>)>,
    /// 現在ゲーム内にログイン中と判断しているUID (全フレンド分)
    pub ingame: HashSet<String>,
    names: HashMap<String, String>,
    pub first_sync: bool,
    /// WebSocketが切れた時刻 (再接続後の同期で、見えていなかった間の離脱を閉じる位置)
    pub ws_down_at: Option<f64>,
    pub mention_on: bool,
}

pub struct Core {
    pub db: Db,
    pub stats: Stats,
    vrc: Arc<dyn VrcApi>,
    discord: Arc<dyn Discord>,
    pub cfg: Config,
    st: Mutex<State>,
    syncing: AtomicBool,
    add_lock: Mutex<()>,
}

fn lock<'a, T>(m: &'a Mutex<T>) -> MutexGuard<'a, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// イベント処理で決まった「通知する内容」(状態を更新したあと、ロックの外で組み立てて送る)。
enum Note {
    Online { name: String, platform: String, loc: Option<String> },
    Offline { name: String },
    Moved { name: String, loc: String },
    Status { name: String, old: Option<(String, String)>, st: String, desc: String },
}

impl Core {
    pub fn new(db: Db, stats: Stats, vrc: Arc<dyn VrcApi>, discord: Arc<dyn Discord>, cfg: Config) -> Self {
        if let Err(e) = i18n::load(&db.c) {
            log::error!("言語設定を読めませんでした: {e}");
        }
        let core = Core {
            db,
            stats,
            vrc,
            discord,
            cfg,
            st: Mutex::new(State {
                last_loc: HashMap::new(),
                last_status: HashMap::new(),
                world_cache: HashMap::new(),
                owner_cache: HashMap::new(),
                ingame: HashSet::new(),
                names: HashMap::new(),
                first_sync: true,
                ws_down_at: None,
                mention_on: true,
            }),
            syncing: AtomicBool::new(false),
            add_lock: Mutex::new(()),
        };
        let on = core.get_setting("mention").map_or(true, |v| v == "1"); // オンライン通知のメンション(既定オン)
        core.state().mention_on = on;
        core
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.st)
    }

    /// WebSocket(pipeline)用のトークン (無効なら再ログインして取り直す)
    pub fn vrc_token(&self) -> Result<String, VrcError> {
        self.vrc.get_token()
    }

    /// 起動時のログイン (2FAが必要なら標準入力で受け付ける)
    pub fn vrc_login(&self) -> Result<(), VrcError> {
        self.vrc.login()
    }

    pub fn is_owner(&self, user: i64) -> bool {
        user == self.cfg.owner
    }

    // ---------- 設定 (bot_settings) ----------
    fn ensure_settings(&self) {
        let _ = crate::db::lock(&self.db.c).execute_batch("CREATE TABLE IF NOT EXISTS bot_settings(k TEXT PRIMARY KEY, v TEXT)");
    }

    pub fn get_setting(&self, k: &str) -> Option<String> {
        self.ensure_settings();
        crate::db::lock(&self.db.c).query_row("SELECT v FROM bot_settings WHERE k=?", [k], |r| r.get::<_, Option<String>>(0)).ok().flatten()
    }

    pub fn set_setting(&self, k: &str, v: &str) {
        self.ensure_settings();
        let _ = crate::db::lock(&self.db.c).execute("INSERT INTO bot_settings(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v", [k, v]);
    }

    pub fn del_setting(&self, k: &str) {
        self.ensure_settings();
        let _ = crate::db::lock(&self.db.c).execute("DELETE FROM bot_settings WHERE k=?", [k]);
    }

    /// そのフレンドにメンションするか。フレンド別の設定(mention:<uid>)があればそれを優先、なければ全体設定。
    pub fn mention_for(&self, uid: &str) -> bool {
        match self.get_setting(&format!("mention:{uid}")) {
            Some(v) => v == "1",
            None => self.state().mention_on,
        }
    }

    pub fn mention_overrides(&self) -> HashMap<String, bool> {
        self.ensure_settings();
        let c = crate::db::lock(&self.db.c);
        let Ok(mut st) = c.prepare("SELECT k,v FROM bot_settings WHERE k LIKE 'mention:%'") else { return HashMap::new() };
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))).map(|it| it.flatten().collect::<Vec<_>>()).unwrap_or_default();
        rows.into_iter().map(|(k, v)| (k["mention:".len()..].to_string(), v.as_deref() == Some("1"))).collect()
    }

    // ---------- 名簿・状態の反映 ----------

    /// 表示名の変更・新しいフレンドを名簿と、登録済みの表示名に反映する。
    fn note_friend_in(&self, st: &mut State, uid: &str, name: Option<&str>, force: bool) {
        let name = name.filter(|n| !n.is_empty());
        if name.is_none() && !force {
            return;
        }
        if !force && st.names.get(uid).map(String::as_str) == name {
            return;
        }
        let row = self.db.get(uid).ok().flatten();
        if let Err(e) = self.stats.upsert_friend(uid, name, row.as_ref().and_then(|r| r.added_at)) {
            log::error!("upsert_friend failed uid={uid}: {e}");
        }
        if let Some(n) = name {
            st.names.insert(uid.to_string(), n.to_string());
            if row.as_ref().map_or(false, |r| r.name.as_deref() != Some(n)) {
                let _ = crate::db::lock(&self.db.c).execute("UPDATE friends SET name=? WHERE uid=?", [n, uid]);
            }
        }
    }

    pub fn note_friend(&self, uid: &str, name: Option<&str>, force: bool) {
        let mut st = self.state();
        self.note_friend_in(&mut st, uid, name, force);
    }

    /// get_user の結果から、ステータスと「ゲーム内にいるか」を復元 (state: online=ゲーム内 / active=Webのみ)
    pub fn seed(&self, uid: &str, u: &UserInfo) {
        let mut st = self.state();
        st.last_status.insert(uid.to_string(), (u.status.clone(), u.desc.clone()));
        if u.state == "online" {
            st.ingame.insert(uid.to_string());
        } else {
            st.ingame.remove(uid);
        }
    }

    /// VRChat上の全フレンドの一覧を取得し、名簿・ゲーム内状態・統計履歴を最新にする(通知は送らない)。
    /// 起動時、WebSocketの再接続のたび、6時間ごとに実行する。
    pub fn sync_friends(&self) {
        if self.syncing.swap(true, Ordering::SeqCst) {
            return; // 同期中の二重実行は無視
        }
        struct Release<'a>(&'a AtomicBool);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _release = Release(&self.syncing);

        let (first, ws_down) = {
            let st = self.state();
            (st.first_sync, st.ws_down_at)
        };
        let close_at = if first { self.stats.prev_alive } else { ws_down }; // 見えていなかった間に離脱した人は見失った時刻で閉じる
        let friends = match self.vrc.get_friends_all() {
            Ok(f) => f,
            Err(e) => {
                log::warn!("friend sync failed: {e:?}");
                return; // 状態(first_sync / ws_down_at)は保持して次回に持ち越す
            }
        };
        let mut st = self.state();
        st.first_sync = false;
        st.ws_down_at = None;
        for f in &friends {
            self.note_friend_in(&mut st, &f.uid, Some(&f.name), true);
            st.last_status.insert(f.uid.clone(), (f.status.clone(), f.desc.clone()));
            if f.ingame {
                st.ingame.insert(f.uid.clone());
                if f.location != "traveling" {
                    st.last_loc.insert(f.uid.clone(), f.location.clone());
                }
            } else {
                st.ingame.remove(&f.uid);
                st.last_loc.remove(&f.uid);
            }
            let _ = self.stats.reconcile(&f.uid, f.ingame, Some(&f.status), close_at);
        }
        let gone = self.stats.missing(&friends.iter().map(|f| f.uid.as_str()).collect::<Vec<_>>()).unwrap_or_default();
        let limit = std::cmp::max(5, friends.len() / 5);
        let skipped = gone.len() > limit;
        if skipped {
            // 一覧の取得不全で大量に「解除」と誤判定しないための安全弁
            log::warn!("friend sync: {} friends missing from the list; skipping unfriend marking", gone.len());
        } else {
            for uid in &gone {
                let _ = self.stats.mark_unfriended(uid);
                st.ingame.remove(uid);
                st.last_loc.remove(uid);
                let _ = self.stats.reconcile(uid, false, None, close_at);
            }
        }
        log::info!("friend sync done: {} friends, {} in game, {} unfriended", friends.len(), friends.iter().filter(|f| f.ingame).count(), if skipped { 0 } else { gone.len() });
    }

    // ---------- 送信 ----------

    pub fn friend_channel(&self, uid: &str, name: &str) -> Result<i64, DiscordError> {
        let existing = self.db.get(uid).ok().flatten().and_then(|r| r.channel_id);
        self.discord.friend_channel(existing, &slug(name, uid), uid)
    }

    /// 登録フレンドのチャンネルに送る (チャンネルが無ければ何もしない)。
    pub fn send_to(&self, uid: &str, content: Option<&str>, embed: &Embed) -> Result<(), DiscordError> {
        match self.db.get(uid).ok().flatten().and_then(|r| r.channel_id) {
            Some(ch) => self.discord.send(ch, content, embed),
            None => Ok(()),
        }
    }

    // ---------- ワールド情報 ----------

    /// インスタンスオーナー名。ユーザー=表示名 / グループ=グループ名。1時間キャッシュ(失敗時は5分)。
    fn owner_name(&self, kind: OwnerKind, oid: &str) -> String {
        if let Some((t, n)) = self.state().owner_cache.get(oid).cloned() {
            if now() - t < if n.is_some() { 3600.0 } else { 300.0 } {
                return n.unwrap_or_else(|| tr!("取得不可"));
            }
        }
        let mut name = None;
        let row = if kind == OwnerKind::Usr { self.db.get(oid).ok().flatten() } else { None };
        if let Some(r) = row {
            name = r.name; // 登録済みフレンドならAPI不要
        } else {
            let r = if kind == OwnerKind::Grp { self.vrc.get_group_name(oid) } else { self.vrc.get_user(oid).map(|u| u.name) };
            match r {
                Ok(n) => name = Some(n),
                Err(e) => log::warn!("owner lookup failed {oid}: {e}"),
            }
        }
        let name = name.filter(|n| !n.is_empty());
        self.state().owner_cache.insert(oid.to_string(), (now(), name.clone()));
        name.unwrap_or_else(|| tr!("取得不可"))
    }

    pub fn world_embed_fields(&self, loc: Option<&str>, e: &mut Embed) {
        let loc_s = loc.unwrap_or("");
        let Some(p) = parse_location(loc_s) else {
            e.add_field(tr!("場所"), if matches!(loc_s, "private" | "") { tr!("Private / 取得不可") } else { loc_s.to_string() }, true);
            return;
        };
        let cached = self.state().world_cache.get(&p.wid).cloned();
        let w = match cached {
            Some((t, info)) if now() - t <= 1800.0 => info,
            _ => {
                let info = self.vrc.get_world(&p.wid).ok();
                self.state().world_cache.insert(p.wid.clone(), (now(), info.clone()));
                info
            }
        };
        e.add_field(tr!("ワールド"), w.as_ref().map_or(p.wid.clone(), |i| format!("[{}](https://vrchat.com/home/world/{})", i.name, p.wid)), false);
        e.add_field(tr!("インスタンス"), format!("{} ({})", p.label, p.region.to_uppercase()), true);
        match self.vrc.get_instance(&p.wid, &p.iid) {
            Ok(i) => {
                let cap = i.capacity.filter(|c| *c != 0).or(w.as_ref().and_then(|w| w.capacity).filter(|c| *c != 0));
                let n = i.n_users.map_or("?".to_string(), |n| n.to_string());
                e.add_field(tr!("人数"), format!("{n}/{}", cap.map_or("?".to_string(), |c| c.to_string())), true);
            }
            Err(_) => e.add_field(tr!("人数"), tr!("取得不可"), true),
        }
        if let Some((kind, oid)) = &p.owner {
            let name = self.owner_name(*kind, oid);
            e.add_field(tr!("オーナー"), if *kind == OwnerKind::Grp { tr!("{name} (グループ)", name = name) } else { name }, true);
        }
        if let Some(info) = &w {
            if let Some(t) = &info.thumb {
                e.thumbnail = Some(t.clone());
            }
            if !info.desc.is_empty() {
                e.add_field(tr!("説明"), info.desc.clone(), false);
            }
        }
    }

    // ---------- WebSocketのイベント ----------

    /// VRChat pipeline の1メッセージを処理する。名簿と統計は全フレンド分を更新し、通知は登録者だけに送る。
    pub fn handle(&self, msg: &Value) {
        let t = msg.get("type").and_then(Value::as_str).unwrap_or("");
        let mut c = msg.get("content").cloned().unwrap_or(Value::Null);
        if let Some(s) = c.as_str() {
            match serde_json::from_str::<Value>(s) {
                Ok(v) => c = v,
                Err(_) => return,
            }
        }
        let Some(c) = c.as_object() else { return };
        let nonempty = |k: &str| c.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
        let Some(uid) = nonempty("userId").or_else(|| nonempty("userid")) else { return }; // friend-active だけキー名が小文字
        let empty = serde_json::Map::new();
        let user = c.get("user").and_then(Value::as_object).unwrap_or(&empty);
        let platform = c.get("platform").and_then(Value::as_str).unwrap_or("");
        let ustr = |k: &str| user.get(k).and_then(Value::as_str);

        let note: Option<Note> = {
            let mut st = self.state();
            if t == "friend-add" {
                self.note_friend_in(&mut st, uid, ustr("displayName"), true);
                return;
            }
            if t == "friend-delete" {
                let _ = self.stats.mark_unfriended(uid);
                st.last_loc.remove(uid);
                if st.ingame.remove(uid) {
                    self.stats.record(uid, "logout", None, None);
                }
                return;
            }
            self.note_friend_in(&mut st, uid, ustr("displayName"), false);
            let row = self.db.get(uid).ok().flatten();
            let notify = row.as_ref().map_or(false, |r| r.active); // 通知するのは /add で登録した人だけ
            let name = row.as_ref().and_then(|r| r.name.clone()).unwrap_or_default();

            // VRChatは「ゲーム内ログイン=friend-online」「Web=friend-active」と別イベントで送る一方、
            // friend-offline はどちらの離脱でも届く。ゲームに入っていた人の離脱だけを扱う。
            match t {
                "friend-online" => {
                    if platform == "web" {
                        return;
                    }
                    st.ingame.insert(uid.to_string());
                    let status = ustr("status").filter(|s| !s.is_empty()).map(str::to_string).or_else(|| st.last_status.get(uid).map(|s| s.0.clone()));
                    self.stats.record(uid, "login", status.as_deref(), None);
                    let loc = c.get("location").and_then(Value::as_str).filter(|s| !s.is_empty()).or_else(|| ustr("location")).map(str::to_string);
                    match &loc {
                        Some(l) => st.last_loc.insert(uid.to_string(), l.clone()),
                        None => st.last_loc.remove(uid),
                    };
                    notify.then(|| Note::Online { name, platform: platform.to_string(), loc })
                }
                "friend-offline" | "friend-active" => {
                    // friend-active(Webのみ)は、ゲームを終了してWebだけ残った場合も含むので「ゲームから抜けた」扱い
                    st.last_loc.remove(uid);
                    if !st.ingame.remove(uid) {
                        return; // Webの入退室など、ゲームに入っていない人の離脱は扱わない
                    }
                    self.stats.record(uid, "logout", None, None);
                    notify.then(|| Note::Offline { name })
                }
                "friend-location" => {
                    let Some(loc) = c.get("location").and_then(Value::as_str).filter(|l| !l.is_empty() && *l != "offline") else { return };
                    if st.ingame.insert(uid.to_string()) {
                        // 実在の場所を持つ=ゲーム内(オンライン通知を取りこぼしていても復元)
                        let status = st.last_status.get(uid).map(|s| s.0.clone());
                        self.stats.record(uid, "resume", status.as_deref(), None);
                    }
                    if loc == "traveling" || st.last_loc.get(uid).map(String::as_str) == Some(loc) {
                        return;
                    }
                    st.last_loc.insert(uid.to_string(), loc.to_string());
                    notify.then(|| Note::Moved { name, loc: loc.to_string() })
                }
                "friend-update" => {
                    let Some(stt) = ustr("status") else { return };
                    let desc = ustr("statusDescription").unwrap_or("").to_string();
                    let old = st.last_status.insert(uid.to_string(), (stt.to_string(), desc.clone()));
                    if st.ingame.contains(uid) && self.stats.state(uid).ok().and_then(|s| s.1).as_deref() != Some(stt) {
                        self.stats.record(uid, "status", Some(stt), None); // ゲーム内でのステータス変更だけ統計に使う
                    }
                    if old.as_ref() == Some(&(stt.to_string(), desc.clone())) || !notify {
                        return;
                    }
                    Some(Note::Status { name, old, st: stt.to_string(), desc })
                }
                _ => None,
            }
        };

        // ---- ここから通信 (ロックの外) ----
        let Some(note) = note else { return };
        let r = match note {
            Note::Online { name, platform, loc } => {
                let mut e = Embed::new().title(tr!("🟢 {name} がオンライン", name = name)).color(0x2ECC71);
                if !platform.is_empty() {
                    e.add_field(tr!("プラットフォーム"), platform, true);
                }
                self.world_embed_fields(loc.as_deref(), &mut e);
                let mention = format!("<@{}>", self.cfg.owner);
                self.send_to(uid, self.mention_for(uid).then_some(mention.as_str()), &e) // メンションはオンラインのみ
            }
            Note::Offline { name } => self.send_to(uid, None, &Embed::new().title(tr!("⚫ {name} がオフライン", name = name)).color(0x95A5A6)),
            Note::Moved { name, loc } => {
                let mut e = Embed::new().title(tr!("🚪 {name} が移動", name = name)).color(0x3498DB);
                self.world_embed_fields(Some(&loc), &mut e);
                self.send_to(uid, None, &e)
            }
            Note::Status { name, old, st, desc } => {
                let (emoji, color) = status_style(&st).unwrap_or(("❔", 0x95A5A6));
                let dash = |d: &str| if d.is_empty() { "-".to_string() } else { d.to_string() };
                let mut e = Embed::new().title(tr!("{emoji} {name} のステータス変更", emoji = emoji, name = name)).color(color);
                if let Some(o) = old {
                    e.add_field(tr!("変更前"), format!("{}\n{}", o.0, dash(&o.1)), true);
                }
                e.add_field(tr!("変更後"), format!("{st}\n{}", dash(&desc)), true);
                self.send_to(uid, None, &e)
            }
        };
        if let Err(e) = r {
            log::error!("event error ({t}): {e}");
        }
    }

    // ---------- 共通の下ごしらえ ----------

    /// メインチャンネルで所有者のみ。拒否なら返答を返す。
    fn guard(&self, user: i64, channel: i64) -> Option<Reply> {
        (user != self.cfg.owner || channel != self.cfg.main_ch).then(|| Reply::private(tr!("メインチャンネルで所有者のみ実行できます。")))
    }

    fn not_owner(&self, user: i64) -> Option<Reply> {
        (user != self.cfg.owner).then(|| Reply::private(tr!("所有者のみ実行できます。")))
    }

    /// VRChat API呼び出し。401なら1回だけ再ログインして再試行。
    fn vrc_call<T>(&self, f: impl Fn(&dyn VrcApi) -> Result<T, VrcError>) -> Result<T, VrcError> {
        match f(&*self.vrc) {
            Err(e) if e.status() == Some(401) => {
                log::warn!("VRChat 401 -> relogin once");
                self.vrc.login()?;
                f(&*self.vrc)
            }
            r => r,
        }
    }

    fn row_in_channel(&self, channel: i64) -> Option<Friend> {
        self.db.all().ok()?.into_iter().find(|r| r.channel_id == Some(channel))
    }

    fn people(rows: impl IntoIterator<Item = (String, Option<String>)>) -> Vec<Person> {
        rows.into_iter().map(|(uid, name)| Person { uid, name }).collect()
    }

    // ---------- /stats ----------

    /// /stats の対象にできる人: 名簿の全員 + 名簿にまだ無い登録済みの人 (削除済み・未登録・フレンド解除済みも含む)
    pub fn who_candidates(&self) -> Vec<Person> {
        let mut known: HashMap<String, Option<String>> = HashMap::new();
        for r in self.stats.friends().unwrap_or_default() {
            known.insert(r.uid, r.name);
        }
        for r in self.db.all().unwrap_or_default() {
            known.entry(r.uid).or_insert(r.name);
        }
        let mut v = Self::people(known);
        v.sort_by(|a, b| {
            let key = |p: &Person| (p.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| p.uid.clone()).to_lowercase(), p.uid.clone());
            key(a).cmp(&key(b))
        });
        v
    }

    pub fn resolve_who(&self, value: &str) -> Result<String, String> {
        let value = value.trim();
        let known = self.who_candidates();
        if let Some(uid) = extract_uid(value) {
            return if known.iter().any(|p| p.uid == uid) { Ok(uid) } else { Err(tr!("該当するフレンドが見つかりません。候補から選択してください。")) };
        }
        let hits: Vec<&Person> = known.iter().filter(|p| p.name.clone().unwrap_or_default().to_lowercase() == value.to_lowercase()).collect();
        match hits.len() {
            1 => Ok(hits[0].uid.clone()),
            0 => Err(tr!("該当するフレンドが見つかりません。候補から選択してください。")),
            _ => Err(tr!("同名のユーザーが複数います。候補一覧から選択してください。")),
        }
    }

    pub fn stats_title(&self, uid: &str) -> String {
        let row = self.db.get(uid).ok().flatten();
        let fr = self.stats.friend(uid).ok().flatten();
        let name = row.as_ref().and_then(|r| r.name.clone()).filter(|n| !n.is_empty()).or_else(|| fr.as_ref().and_then(|f| f.name.clone()).filter(|n| !n.is_empty())).unwrap_or_else(|| uid.to_string());
        let mut tag = match &row {
            Some(r) if r.active => String::new(),
            None => tr!(" (未登録)"),
            Some(_) => tr!(" (削除済み)"),
        };
        if fr.as_ref().map_or(false, |f| !f.is_friend) {
            tag += &tr!(" (フレンド解除済み)");
        }
        tr!("📊 {name} のログイン統計", name = name) + &tag
    }

    pub fn cmd_stats(&self, user: i64, channel: i64, days: Option<i64>, who: Option<&str>) -> Reply {
        if let Some(r) = self.not_owner(user) {
            return r;
        }
        let t = now();
        let cutoff = days.filter(|d| *d != 0).map_or(0.0, |d| t - d as f64 * 86400.0);
        let mut names: Option<Vec<(String, Option<String>)>> = None;
        let (uids, title): (Vec<String>, String);
        if let Some(w) = who.filter(|w| !w.is_empty()) {
            // 指定したフレンドだけ (登録の有無に関係なく、収集済みのデータで分析できる)
            match self.resolve_who(w) {
                Err(e) => return Reply::private(e),
                Ok(u) => {
                    title = self.stats_title(&u);
                    uids = vec![u];
                }
            }
        } else if channel == self.cfg.main_ch {
            // メインチャンネル: 登録中の全フレンド
            let n: Vec<(String, Option<String>)> = self.db.active().unwrap_or_default().into_iter().map(|r| (r.uid, r.name)).collect();
            if n.is_empty() {
                return Reply::text(tr!("登録フレンドがいません。"));
            }
            uids = n.iter().map(|x| x.0.clone()).collect();
            title = tr!("📊 ログイン統計 (登録フレンド全員)");
            names = Some(n);
        } else {
            // フレンド用チャンネル: そのフレンドだけ
            let Some(row) = self.row_in_channel(channel) else { return Reply::private(tr!("メインチャンネルか、フレンド用チャンネルで実行してください。")) };
            title = self.stats_title(&row.uid);
            uids = vec![row.uid];
        }
        let start = cutoff.max(self.stats.tracked_since(&uids).ok().flatten().unwrap_or(0.0)); // 収集開始日時はフレンドごと
        match self.stats.summarize(&uids, start, t) {
            Ok(res) => Reply::embed(stats_embed(&title, &res, start, t, self.stats.tz(), names.as_deref())),
            Err(e) => {
                log::error!("stats failed: {e}");
                Reply::private(tr!("VRChat APIでエラーが発生しました。ログを確認してください。"))
            }
        }
    }

    // ---------- /lang ----------

    pub fn cmd_lang(&self, user: i64, code: &str) -> Reply {
        if let Some(r) = self.not_owner(user) {
            return r;
        }
        let Some(language) = i18n::resolve(code) else {
            let codes = i18n::langs().iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ");
            return Reply::private(tr!("未対応の言語です。使用できるコード: {codes}", codes = codes));
        };
        let _ = i18n::set_lang(language);
        if let Err(e) = i18n::save(&self.db.c, language) {
            log::error!("言語設定を保存できませんでした: {e}"); // 再起動後は戻ってしまう
        }
        log::info!("language switched by={user} lang={language}");
        let name = i18n::langs().iter().find(|(k, _)| *k == language).map_or("", |(_, n)| *n);
        Reply::text(tr!("言語を {name} に切り替えました。", name = name))
    }

    // ---------- /mention ----------

    pub fn cmd_mention(&self, user: i64, channel: i64, state: &str) -> Reply {
        if let Some(r) = self.not_owner(user) {
            return r;
        }
        if !matches!(state, "on" | "off" | "reset") {
            return Reply::private(tr!("on / off / reset を指定してください。"));
        }
        let on = state == "on";
        if channel == self.cfg.main_ch {
            // メインチャンネル: 全体設定 (フレンド別の設定がないフレンドに適用)
            if state == "reset" {
                return Reply::private(tr!("reset はフレンド用チャンネルで使います。"));
            }
            self.state().mention_on = on;
            self.set_setting("mention", if on { "1" } else { "0" }); // 再起動後も保持
            log::info!("online mention (global) set by={user} state={state}");
            return Reply::text(if on { tr!("🔔 オンライン通知のメンション(全体設定)をオンにしました。") } else { tr!("🔕 オンライン通知のメンション(全体設定)をオフにしました。") });
        }
        let Some(row) = self.row_in_channel(channel) else { return Reply::private(tr!("メインチャンネルか、フレンド用チャンネルで実行してください。")) };
        let name = esc(row.name.as_deref().unwrap_or(""));
        if state == "reset" {
            self.del_setting(&format!("mention:{}", row.uid));
            log::info!("online mention reset by={user} uid={}", row.uid);
            let cur = if self.state().mention_on { tr!("オン") } else { tr!("オフ") };
            return Reply::text(tr!("↩️ {name} のメンションを全体設定に戻しました (現在: {state})。", name = name, state = cur));
        }
        self.set_setting(&format!("mention:{}", row.uid), if on { "1" } else { "0" });
        log::info!("online mention set by={user} uid={} state={state}", row.uid);
        Reply::text(if on { tr!("🔔 {name} のオンライン通知のメンションをオンにしました。", name = name) } else { tr!("🔕 {name} のオンライン通知のメンションをオフにしました。", name = name) })
    }

    // ---------- /add ----------

    /// /add の候補: VRChat上のフレンドのうち、まだ通知登録していない人(削除済みも再登録できる)
    pub fn add_candidates(&self) -> Vec<VrcFriend> {
        let active: HashSet<String> = self.db.active().unwrap_or_default().into_iter().map(|r| r.uid).collect();
        self.stats.friends().unwrap_or_default().into_iter().filter(|r| r.is_friend && !active.contains(&r.uid)).collect()
    }

    /// 入力(候補のUID・プロフィールURL・名前)から対象UIDを決める。
    pub fn resolve_add(&self, value: &str) -> Result<String, String> {
        let value = value.trim();
        if let Some(uid) = extract_uid(value) {
            return Ok(uid);
        }
        let hits: Vec<VrcFriend> = self.add_candidates().into_iter().filter(|r| r.name.clone().unwrap_or_default().to_lowercase() == value.to_lowercase()).collect();
        match hits.len() {
            1 => Ok(hits[0].uid.clone()),
            0 => Err(tr!("該当するフレンドが見つかりません。候補から選ぶか、プロフィールURL (`https://vrchat.com/home/user/usr_...`) を指定してください。")),
            _ => Err(tr!("同名のユーザーが複数います。候補一覧から選択してください。")),
        }
    }

    /// 通信前の確認。拒否/入力エラーなら返答(Err)、通れば対象UID(Ok)。
    pub fn cmd_add_prepare(&self, user: i64, channel: i64, target: &str) -> Result<String, Reply> {
        if let Some(r) = self.guard(user, channel) {
            return Err(r);
        }
        self.resolve_add(target).map_err(Reply::text)
    }

    /// 追加の本処理 (VRChat APIとチャンネル作成を伴うので、defer 後に呼ぶ)。
    pub fn cmd_add_run(&self, user: i64, uid: &str) -> Reply {
        let known = self.stats.friend(uid).ok().flatten();
        let (u, name): (Option<UserInfo>, String) = match known {
            Some(k) if k.is_friend && k.name.as_deref().map_or(false, |n| !n.is_empty()) => (None, k.name.unwrap_or_default()), // 名簿にいる人はAPI不要
            _ => match self.vrc_call(|v| v.get_user(uid)) {
                // 名簿にいない人(URL指定)は存在確認も兼ねてAPIで取得
                Ok(u) => {
                    let n = u.name.clone();
                    (Some(u), n)
                }
                Err(e) => {
                    log::warn!("add: get_user failed uid={uid}: {e:?}");
                    return Reply::text(tr!("❌ {msg}", msg = explain_error(&e)));
                }
            },
        };
        let _guard = lock(&self.add_lock); // 同時追加でチャンネルが二重作成されるのを防ぐ
        let existing = self.db.get(uid).ok().flatten();
        if let Some(ex) = existing.as_ref().filter(|e| e.active) {
            return Reply::text(tr!("すでに登録済みです: **{name}** <#{ch}>", name = esc(ex.name.as_deref().unwrap_or("")), ch = ex.channel_id.unwrap_or(0)));
        }
        let ch = match self.friend_channel(uid, &name) {
            Ok(c) => c,
            Err(e) => {
                log::error!("add: channel creation failed uid={uid}: {e}");
                return Reply::text(tr!("❌ チャンネル作成に失敗しました (Botの権限を確認): {status}", status = e.status.unwrap_or(0)));
            }
        };
        if let Err(e) = self.db.upsert(uid, &name, ch) {
            log::error!("add: db upsert failed: {e}");
            return Reply::text(tr!("❌ {msg}", msg = e.to_string()));
        }
        let mut warn = String::new();
        if let Some(u) = &u {
            self.seed(uid, u);
            if u.is_friend {
                self.note_friend(uid, Some(&name), true); // 名簿に載せて、以後は全フレンドと同じく収集する
                let ingame = self.state().ingame.contains(uid);
                let _ = self.stats.reconcile(uid, ingame, Some(&u.status), None);
            } else {
                warn = tr!("\n⚠️ このアカウントとフレンドではないため、通知は届きません。");
            }
        }
        log::info!("friend added by={user} uid={uid} name={name} rejoin={}", existing.is_some());
        let e = Embed::new()
            .title(tr!("➕ メンバー追加") + &(if existing.is_some() { tr!("(再登録)") } else { String::new() }))
            .description(format!("**{}**\n`{uid}`", esc(&name)))
            .color(0x2ECC71);
        if let Err(err) = self.discord.send(ch, None, &e) {
            log::error!("add: failed to write log to member channel: {err}");
        }
        Reply { content: Some(tr!("✅ 追加しました: **{name}** → {ch}{warn}", name = esc(&name), ch = format!("<#{ch}>"), warn = warn)), embed: Some(e), ..Default::default() }
    }

    // ---------- /remove ----------

    /// オートコンプリート値(UID)から登録行を探す。手入力された名前も、一意なら許容。
    pub fn resolve_remove(&self, value: &str) -> Result<Friend, String> {
        let value = value.trim();
        if let Some(uid) = extract_uid(value) {
            return match self.db.get(&uid).ok().flatten() {
                Some(r) if r.active => Ok(r),
                _ => Err(tr!("その登録は存在しません。")),
            };
        }
        let mut hits: Vec<Friend> = self.db.active().unwrap_or_default().into_iter().filter(|r| r.name.clone().unwrap_or_default().to_lowercase() == value.to_lowercase()).collect();
        match hits.len() {
            1 => Ok(hits.remove(0)),
            0 => Err(tr!("その登録は存在しません。候補一覧から選択してください。")),
            _ => Err(tr!("同名のユーザーが複数います。候補一覧から選択してください。")),
        }
    }

    pub fn cmd_remove(&self, user: i64, channel: i64, target: &str) -> Reply {
        if let Some(r) = self.guard(user, channel) {
            return r;
        }
        let row = match self.resolve_remove(target) {
            Ok(r) => r,
            Err(e) => return Reply::text(e),
        };
        let e = Embed::new().title(tr!("このメンバーを削除しますか？")).color(0xE67E22).description(tr!(
            "**{name}**\n`{uid}`\nチャンネル: <#{ch}> (削除されません)",
            name = esc(row.name.as_deref().unwrap_or("")),
            uid = row.uid.clone(),
            ch = row.channel_id.unwrap_or(0)
        ));
        Reply { embed: Some(e), view: Some(View::Confirm { uid: row.uid, name: row.name.unwrap_or_default() }), ..Default::default() }
    }

    /// 確認ボタンの権限チェック (所有者のみ操作可)
    pub fn view_check(&self, user: i64) -> Option<Reply> {
        (user != self.cfg.owner).then(|| Reply::private(tr!("所有者のみ操作できます。")))
    }

    /// 「削除する」ボタン。通知をやめるだけで、統計の収集(名簿・ゲーム内状態)は続ける。
    pub fn confirm_yes(&self, user: i64, uid: &str) -> Reply {
        let row = match self.db.get(uid).ok().flatten() {
            Some(r) if r.active => r,
            _ => return Reply::text(tr!("すでに削除されています。")),
        };
        let _ = self.db.deactivate(uid);
        let name = row.name.clone().unwrap_or_default();
        log::info!("friend removed by={user} uid={uid} name={name}");
        let e = Embed::new().title(tr!("➖ メンバー削除")).description(format!("**{}**\n`{uid}`", esc(&name))).color(0xE74C3C);
        if let Err(err) = self.send_to(uid, None, &e) {
            log::error!("failed to write removal log to member channel uid={uid}: {err}"); // メンバーチャンネルにも履歴を残す
        }
        Reply {
            content: Some(tr!("🗑️ **{name}** を削除しました (チャンネルは保持): <#{ch}>", name = esc(&name), ch = row.channel_id.unwrap_or(0))),
            embed: Some(e),
            ..Default::default()
        }
    }

    pub fn confirm_no(&self, name: &str) -> Reply {
        Reply::text(tr!("キャンセルしました: {name}", name = esc(name)))
    }

    pub fn confirm_timeout(&self) -> Reply {
        Reply::text(tr!("確認がタイムアウトしました。削除していません。"))
    }

    // ---------- /list ----------

    pub fn cmd_list(&self, user: i64, channel: i64, show_uid: bool, include_removed: bool) -> Reply {
        if let Some(r) = self.guard(user, channel) {
            return r;
        }
        let rows = if include_removed { self.db.all() } else { self.db.active() }.unwrap_or_default();
        if rows.is_empty() {
            return Reply::text(tr!("登録なし"));
        }
        let mention_on = self.state().mention_on;
        let view = ListView::new(rows, show_uid, Some(mention_on), self.mention_overrides());
        let embed = view.embed();
        if view.pages == 1 {
            return Reply::embed(embed);
        }
        Reply { embed: Some(embed), view: Some(View::List(view)), ..Default::default() }
    }

    // ---------- オートコンプリート (autocompleteには権限チェックが効かないので明示的に所有者だけに) ----------

    pub fn ac_who(&self, user: i64, current: &str) -> Vec<Choice> {
        if !self.is_owner(user) {
            return vec![];
        }
        make_choices(&self.who_candidates(), current)
    }

    pub fn ac_add(&self, user: i64, current: &str) -> Vec<Choice> {
        if !self.is_owner(user) {
            return vec![];
        }
        make_choices(&Self::people(self.add_candidates().into_iter().map(|r| (r.uid, r.name))), current)
    }

    pub fn ac_remove(&self, user: i64, current: &str) -> Vec<Choice> {
        if !self.is_owner(user) {
            return vec![];
        }
        make_choices(&Self::people(self.db.active().unwrap_or_default().into_iter().map(|r| (r.uid, r.name))), current)
    }
}
