//! vrc.py の移植: VRChat REST API のラッパー。
//!
//! Python版は同期の vrchatapi SDK を `asyncio.to_thread` で呼んでいたので、ここも同期クライアント(ureq)で書き、
//! Bot側からは `tokio::task::spawn_blocking` で呼ぶ想定。使うAPIは次の8本だけ:
//!   GET /auth/user, POST /auth/twofactorauth/{totp,emailotp}/verify, GET /auth,
//!   GET /users/{id}, /worlds/{id}, /instances/{wid}:{iid}, /groups/{id}, /auth/user/friends
//! 保存済みトークン `.vrc_auth` はPython版と同じ形式 (auth クッキーの値だけ) なので、そのまま引き継げる。
use crate::tr;
use serde_json::Value;
use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

pub const TOKEN_FILE: &str = ".vrc_auth";
pub const DEFAULT_BASE: &str = "https://api.vrchat.cloud/api/1";

// ---------- UID / URL ----------

const UID_LEN: usize = 40; // usr_ + 8-4-4-4-12 (ハイフン込み 36)

/// `usr_xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` (大文字小文字を無視) の完全一致か。
fn is_uid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != UID_LEN || !b[..4].eq_ignore_ascii_case(b"usr_") {
        return false;
    }
    let groups = [(4, 12), (13, 17), (18, 22), (23, 27), (28, 40)];
    let dashes = [12, 17, 22, 27];
    dashes.iter().all(|&i| b[i] == b'-') && groups.iter().all(|&(a, z)| b[a..z].iter().all(u8::is_ascii_hexdigit))
}

/// プロフィールURL (https://vrchat.com/home/user/usr_...) または usr_... からUIDを取り出す。
/// 不正なら None。ホストは完全一致(userinfo/ポート付きは拒否)、クエリ/フラグメントは無視。
pub fn extract_uid(text: &str) -> Option<String> {
    let s = text.trim().trim_matches(['<', '>']).trim();
    if s.is_empty() || s.chars().count() > 300 {
        return None;
    }
    if is_uid(s) {
        return Some(s.to_ascii_lowercase());
    }
    // Python の urlparse と同じ切り分け: scheme://netloc/path;params?query#fragment
    let s: String = s.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect(); // urlparse は制御文字を捨てる
    let (scheme, rest) = s.split_once(':')?;
    if scheme.is_empty()
        || !scheme.chars().next()?.is_ascii_alphabetic()
        || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return None;
    }
    let rest = rest.strip_prefix("//")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (netloc, tail) = rest.split_at(end);
    if !matches!(netloc.to_ascii_lowercase().as_str(), "vrchat.com" | "www.vrchat.com") {
        return None;
    }
    let path = &tail[..tail.find(['?', '#']).unwrap_or(tail.len())];
    // ;params は最後のセグメントからだけ切り離される (urlparse の _splitparams)
    let path = match path.rfind('/').and_then(|slash| path[slash..].find(';').map(|i| slash + i)) {
        Some(i) => &path[..i],
        None => path,
    };
    let lower = path.to_ascii_lowercase();
    let id = lower.strip_prefix("/home/user/")?;
    let id = id.strip_suffix('/').unwrap_or(id);
    if is_uid(id) {
        Some(id.to_string())
    } else {
        None
    }
}

// ---------- エラー ----------

#[derive(Debug)]
pub enum VrcError {
    /// HTTPエラー (Python SDK の ApiException.status 相当)
    Status(u16),
    /// 応答が時間内に来なかった
    Timeout,
    /// 2FAの入力が必要なのに入力できない (Python版の EOFError)
    NeedsReauth,
    /// 通信そのものの失敗
    Transport(String),
    /// 想定外の応答
    Parse(String),
}

impl VrcError {
    pub fn status(&self) -> Option<u16> {
        match self {
            VrcError::Status(s) => Some(*s),
            _ => None,
        }
    }
}

impl std::fmt::Display for VrcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VrcError::Status(s) => write!(f, "HTTP {s}"),
            VrcError::Timeout => write!(f, "timeout"),
            VrcError::NeedsReauth => write!(f, "2FA input required"),
            VrcError::Transport(m) => write!(f, "transport error: {m}"),
            VrcError::Parse(m) => write!(f, "unexpected response: {m}"),
        }
    }
}
impl StdError for VrcError {}

/// 例外 -> ユーザー向けメッセージ(内部情報や秘密は含めない)
pub fn explain_error(e: &VrcError) -> String {
    match e {
        VrcError::Timeout => return tr!("VRChat APIの応答がタイムアウトしました。少し待って再試行してください。"),
        VrcError::NeedsReauth => {
            return tr!("VRChatの再認証(2FA)が必要です。サーバーで `python bot.py` を手動起動して認証してください。")
        }
        _ => {}
    }
    match e.status() {
        Some(404) => tr!("そのユーザーはVRChat上に存在しません。URLを確認してください。"),
        Some(401) | Some(403) => tr!("VRChat認証エラーです。サーバー側で再認証が必要な可能性があります。"),
        Some(429) => tr!("VRChat APIのレート制限中です。しばらく待って再試行してください。"),
        Some(st) if st >= 500 => tr!("VRChat側のサーバーエラーです (HTTP {st})。", st = st),
        _ => tr!("VRChat APIでエラーが発生しました。ログを確認してください。"),
    }
}

// ---------- 戻り値 ----------

#[derive(Debug, Clone, PartialEq)]
pub struct UserInfo {
    pub name: String,
    pub status: String,
    pub desc: String,
    pub is_friend: bool,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorldInfo {
    pub name: String,
    pub author: String,
    pub capacity: Option<i64>,
    pub thumb: Option<String>,
    pub desc: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceInfo {
    pub n_users: Option<i64>,
    pub capacity: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FriendEntry {
    pub uid: String,
    pub name: String,
    pub status: String,
    pub desc: String,
    pub location: String,
    pub ingame: bool,
}

// ---------- 小さな部品 ----------

/// URLエンコード (予約されていない文字 A-Z a-z 0-9 - _ . ~ だけをそのまま残す)。
fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

pub(crate) fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::with_capacity(input.len().div_ceil(3) * 4);
    for c in input.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        o.push(T[(n >> 18) as usize & 63] as char);
        o.push(T[(n >> 12) as usize & 63] as char);
        o.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        o.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    o
}

fn is_timeout(t: &ureq::Transport) -> bool {
    let mut src: Option<&(dyn StdError + 'static)> = Some(t);
    while let Some(e) = src {
        if let Some(io) = e.downcast_ref::<io::Error>() {
            if matches!(io.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) {
                return true;
            }
        }
        src = e.source();
    }
    false
}

fn map_err(e: ureq::Error) -> VrcError {
    match e {
        ureq::Error::Status(code, _) => VrcError::Status(code),
        ureq::Error::Transport(t) if is_timeout(&t) => VrcError::Timeout,
        ureq::Error::Transport(t) => VrcError::Transport(t.to_string()),
    }
}

fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

fn nonempty(v: &Value, k: &str) -> String {
    str_of(v, k).unwrap_or_default()
}

/// GET /auth/user の結果
enum Me {
    Ready,
    /// requiresTwoFactorAuth の中身 (例: ["emailOtp"] / ["totp", "otp"])
    Needs2fa(Vec<String>),
}

#[derive(Default)]
struct Session {
    cookies: BTreeMap<String, String>,
    token: Option<String>,
}

type Prompt = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

fn build_agent(ua: &str, timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(ua)
        .timeout_connect(Duration::from_secs(10))
        .timeout(timeout)
        .redirects(0)
        .build()
}

pub struct Vrc {
    ua: String,
    username: String,
    password: String,
    base: String,
    agent: ureq::Agent,
    session: Mutex<Session>,
    token_file: PathBuf,
    prompt: Prompt,
    page_delay: Duration,
}

impl Vrc {
    pub fn new(username: &str, password: &str, user_agent: &str) -> Self {
        Vrc {
            ua: user_agent.to_string(),
            agent: build_agent(user_agent, Duration::from_secs(20)),
            username: username.to_string(),
            password: password.to_string(),
            base: DEFAULT_BASE.to_string(),
            session: Mutex::new(Session::default()),
            token_file: PathBuf::from(TOKEN_FILE),
            prompt: Box::new(|msg| {
                // Python版の input() 相当。標準入力が閉じていれば None (= 再認証が必要)
                print!("{msg}");
                io::stdout().flush().ok();
                let mut line = String::new();
                match io::stdin().read_line(&mut line) {
                    Ok(0) | Err(_) => None,
                    Ok(_) => Some(line.trim().to_string()),
                }
            }),
            page_delay: Duration::from_millis(500),
        }
    }

    pub fn with_base(mut self, base: &str) -> Self {
        self.base = base.trim_end_matches('/').to_string();
        self
    }
    pub fn with_token_file(mut self, p: impl Into<PathBuf>) -> Self {
        self.token_file = p.into();
        self
    }
    pub fn with_prompt(mut self, f: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        self.prompt = Box::new(f);
        self
    }
    pub fn with_page_delay(mut self, d: Duration) -> Self {
        self.page_delay = d;
        self
    }
    /// 1リクエストの上限時間を変える (既定20秒)。
    pub fn with_timeout(mut self, d: Duration) -> Self {
        self.agent = build_agent(&self.ua, d);
        self
    }

    fn sess(&self) -> std::sync::MutexGuard<'_, Session> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn cookie_header(&self) -> Option<String> {
        let s = self.sess();
        if s.cookies.is_empty() {
            return None;
        }
        Some(s.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; "))
    }

    fn store_cookies(&self, resp: &ureq::Response) {
        let mut s = self.sess();
        for h in resp.all("set-cookie") {
            let first = h.split(';').next().unwrap_or("");
            if let Some((k, v)) = first.split_once('=') {
                let (k, v) = (k.trim(), v.trim());
                if v.is_empty() {
                    s.cookies.remove(k);
                } else {
                    s.cookies.insert(k.to_string(), v.to_string());
                }
            }
        }
    }

    fn basic_header(&self) -> String {
        format!("Basic {}", base64(format!("{}:{}", enc(&self.username), enc(&self.password)).as_bytes()))
    }

    fn call(&self, method: &str, path: &str, query: &[(&str, String)], body: Option<Value>, basic: bool) -> Result<Value, VrcError> {
        let mut req = self.agent.request(method, &format!("{}{}", self.base, path));
        for (k, v) in query {
            req = req.query(k, v);
        }
        if let Some(c) = self.cookie_header() {
            req = req.set("Cookie", &c);
        }
        if basic {
            req = req.set("Authorization", &self.basic_header());
        }
        let resp = match body {
            Some(b) => req.set("Content-Type", "application/json").send_string(&b.to_string()),
            None => req.call(),
        }
        .map_err(map_err)?;
        self.store_cookies(&resp);
        let text = resp.into_string().map_err(|e| {
            if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) {
                VrcError::Timeout
            } else {
                VrcError::Transport(e.to_string())
            }
        })?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| VrcError::Parse(e.to_string()))
    }

    fn get(&self, path: &str) -> Result<Value, VrcError> {
        self.call("GET", path, &[], None, false)
    }

    // ---------- 認証 ----------

    fn current_user(&self, basic: bool) -> Result<Me, VrcError> {
        let v = self.call("GET", "/auth/user", &[], None, basic)?;
        match v.get("requiresTwoFactorAuth").and_then(Value::as_array) {
            Some(a) => Ok(Me::Needs2fa(a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())),
            None => Ok(Me::Ready),
        }
    }

    fn verify_2fa(&self, path: &str, code: String) -> Result<(), VrcError> {
        let v = self.call("POST", path, &[], Some(serde_json::json!({ "code": code })), false)?;
        match v.get("verified").and_then(Value::as_bool) {
            Some(false) => Err(VrcError::Status(401)),
            _ => Ok(()),
        }
    }

    pub fn login(&self) -> Result<(), VrcError> {
        // 保存済みトークンがあれば再利用
        if let Ok(saved) = std::fs::read_to_string(&self.token_file) {
            let saved = saved.trim().to_string();
            self.sess().cookies.insert("auth".into(), saved.clone());
            if matches!(self.current_user(false), Ok(Me::Ready)) {
                self.sess().token = Some(saved);
                return Ok(());
            }
            self.sess().cookies.remove("auth");
        }
        match self.current_user(true)? {
            Me::Ready => {}
            Me::Needs2fa(kinds) => {
                if kinds.iter().any(|k| k == "emailOtp") {
                    let code = (self.prompt)("メール2FAコード: ").ok_or(VrcError::NeedsReauth)?;
                    self.verify_2fa("/auth/twofactorauth/emailotp/verify", code.trim().to_string())?;
                } else if kinds.iter().any(|k| k == "totp" || k == "otp") {
                    let code = (self.prompt)("認証アプリの2FAコード: ").ok_or(VrcError::NeedsReauth)?;
                    self.verify_2fa("/auth/twofactorauth/totp/verify", code.trim().to_string())?;
                } else {
                    return Err(VrcError::Parse(format!("unknown 2FA method: {kinds:?}")));
                }
                if let Me::Needs2fa(_) = self.current_user(true)? {
                    return Err(VrcError::NeedsReauth);
                }
            }
        }
        self.refresh_token()?;
        Ok(())
    }

    fn verify_auth_token(&self) -> Result<String, VrcError> {
        let v = self.get("/auth")?;
        match (v.get("ok").and_then(Value::as_bool), str_of(&v, "token")) {
            (Some(false), _) | (_, None) => Err(VrcError::Status(401)),
            (_, Some(t)) => Ok(t),
        }
    }

    pub fn refresh_token(&self) -> Result<String, VrcError> {
        let token = self.verify_auth_token()?;
        self.sess().token = Some(token.clone());
        write_secret(&self.token_file, &token).map_err(|e| VrcError::Transport(format!("token file: {e}")))?;
        Ok(token)
    }

    /// WebSocket(pipeline) 用のトークン。無効なら再ログインして取り直す。
    pub fn get_token(&self) -> Result<String, VrcError> {
        match self.verify_auth_token() {
            Ok(_) => {
                let t = self.sess().token.clone();
                match t {
                    Some(t) => Ok(t),
                    None => self.refresh_token(),
                }
            }
            Err(_) => {
                self.login()?;
                self.sess().token.clone().ok_or(VrcError::NeedsReauth)
            }
        }
    }

    // ---------- 参照系 ----------

    pub fn get_user(&self, uid: &str) -> Result<UserInfo, VrcError> {
        let u = self.get(&format!("/users/{}", enc(uid)))?;
        Ok(UserInfo {
            name: nonempty(&u, "displayName"),
            status: nonempty(&u, "status"),
            desc: nonempty(&u, "statusDescription"),
            is_friend: u.get("isFriend").and_then(Value::as_bool).unwrap_or(false),
            state: nonempty(&u, "state"),
        })
    }

    pub fn get_world(&self, wid: &str) -> Result<WorldInfo, VrcError> {
        let w = self.get(&format!("/worlds/{}", enc(wid)))?;
        Ok(WorldInfo {
            name: nonempty(&w, "name"),
            author: nonempty(&w, "authorName"),
            capacity: w.get("capacity").and_then(Value::as_i64),
            thumb: str_of(&w, "thumbnailImageUrl"),
            desc: nonempty(&w, "description").chars().take(200).collect(),
        })
    }

    pub fn get_instance(&self, wid: &str, iid: &str) -> Result<InstanceInfo, VrcError> {
        let i = self.get(&format!("/instances/{}:{}", enc(wid), enc(iid)))?;
        Ok(InstanceInfo {
            n_users: i.get("n_users").and_then(Value::as_i64),
            capacity: i.get("capacity").and_then(Value::as_i64),
        })
    }

    pub fn get_group_name(&self, gid: &str) -> Result<String, VrcError> {
        let g = self.get(&format!("/groups/{}", enc(gid)))?;
        str_of(&g, "name").ok_or_else(|| VrcError::Parse("group has no name".into()))
    }

    /// このアカウントのフレンド全員(オンライン側+オフライン側)を、100件ずつ取得して返す。
    /// 現在地が offline 以外ならゲーム内(ingame)。Webだけの人は location が offline なのでゲーム内にならない。
    pub fn get_friends_all(&self) -> Result<Vec<FriendEntry>, VrcError> {
        let mut out = vec![];
        let mut seen = std::collections::HashSet::new();
        for offline in [false, true] {
            let mut offset = 0usize;
            loop {
                let page = self.call(
                    "GET",
                    "/auth/user/friends",
                    &[("offset", offset.to_string()), ("n", "100".into()), ("offline", offline.to_string())],
                    None,
                    false,
                )?;
                let items = page.as_array().ok_or_else(|| VrcError::Parse("friends is not an array".into()))?;
                for f in items {
                    let Some(id) = str_of(f, "id") else { continue };
                    if !seen.insert(id.clone()) {
                        continue;
                    }
                    let loc = Some(nonempty(f, "location")).filter(|l| !l.is_empty()).unwrap_or_else(|| "offline".into());
                    out.push(FriendEntry {
                        uid: id,
                        name: nonempty(f, "displayName"),
                        status: nonempty(f, "status"),
                        desc: nonempty(f, "statusDescription"),
                        ingame: loc != "offline",
                        location: loc,
                    });
                }
                if items.len() < 100 {
                    break;
                }
                offset += 100;
                std::thread::sleep(self.page_delay); // レート制限に配慮
            }
            std::thread::sleep(self.page_delay);
        }
        Ok(out)
    }
}

/// 秘密(トークン)を書く。Unixでは所有者だけが読めるようにする。
fn write_secret(path: &std::path::Path, text: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        f.write_all(text.as_bytes())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, text)
    }
}
