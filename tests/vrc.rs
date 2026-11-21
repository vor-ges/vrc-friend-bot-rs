//! test_local.py の extract_uid / explain_error の移植と、簡易HTTPサーバーを使った認証・API呼び出しのテスト。
//! 本物のVRChatには繋がない (繋いだ確認は別途)。
mod common;
use common::{spawn, Mock, Req, Resp};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vrc_friend_bot_rs::i18n;
use vrc_friend_bot_rs::vrc::{explain_error, extract_uid, Vrc, VrcError};

const U: &str = "usr_12345678-1234-1234-1234-123456789abc";
const BASIC: &str = "Basic YWxpY2UlNDBleGFtcGxlLmNvbTpwJTQwc3MlMjB3MHJkJTI1"; // alice@example.com / "p@ss w0rd%" をURLエンコード→base64

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("vrcrs-{}-{}-{name}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let _ = std::fs::remove_file(&p);
    p
}

fn client(m: &Mock, token_file: &std::path::Path) -> Vrc {
    Vrc::new("alice@example.com", "p@ss w0rd%", "TestBot/1.0 test@example.com")
        .with_base(&m.base)
        .with_token_file(token_file)
        .with_page_delay(Duration::ZERO)
        .with_prompt(|_| None)
}

fn path_of(r: &Req) -> &str {
    r.path.split('?').next().unwrap()
}

// ---------- extract_uid / explain_error ----------

#[test]
fn extract_uid_accepts_and_rejects() {
    let upper_hex = U.to_uppercase().replace("USR_", "usr_");
    let ok = [
        format!("https://vrchat.com/home/user/{U}"),
        format!("https://www.vrchat.com/home/user/{U}/"),
        format!("http://vrchat.com/home/user/{U}?x=1#y"),
        format!("<https://vrchat.com/home/user/{U}>"),
        format!("  {U}  "),
        upper_hex,
    ];
    for s in &ok {
        assert_eq!(extract_uid(s).as_deref(), Some(U), "{s}");
    }
    let bad = [
        "".to_string(),
        "abc".into(),
        "usr_123".into(),
        format!("https://evil.com/home/user/{U}"),
        format!("https://vrchat.com.evil.com/home/user/{U}"),
        format!("https://vrchat.com@evil.com/home/user/{U}"),
        format!("https://evil.com@vrchat.com/home/user/{U}"),
        format!("https://vrchat.com:8080/home/user/{U}"),
        format!("https://vrchat.com/home/world/{U}"),
        format!("https://vrchat.com/home/user/{U}/extra"),
        format!("ftp://vrchat.com/home/user/{U}"),
        "https://vrchat.com/home/user/usr_zzzz".into(),
        "x".repeat(400),
    ];
    for s in &bad {
        assert_eq!(extract_uid(s), None, "{s}");
    }
}

#[test]
fn explain_error_messages() {
    // 言語はグローバル状態なので、explain_error を呼ぶテストはこの1つだけにする
    i18n::set_lang("ja").unwrap();
    assert!(explain_error(&VrcError::Status(404)).contains("存在しません"));
    assert!(explain_error(&VrcError::Status(401)).contains("認証"));
    assert!(explain_error(&VrcError::Status(403)).contains("認証"));
    assert!(explain_error(&VrcError::Status(429)).contains("レート"));
    assert!(explain_error(&VrcError::Timeout).contains("タイムアウト"));
    assert!(explain_error(&VrcError::NeedsReauth).contains("2FA"));
    assert!(explain_error(&VrcError::Status(503)).contains("HTTP 503"));
    assert!(explain_error(&VrcError::Status(400)).contains("エラーが発生"));
    assert!(explain_error(&VrcError::Transport("secret-host".into())).contains("エラーが発生"));
    assert!(!explain_error(&VrcError::Transport("secret-host".into())).contains("secret")); // 内部情報は出さない
    i18n::set_lang("en").unwrap();
    assert!(explain_error(&VrcError::Status(500)).contains("server error (HTTP 500)"));
    assert!(explain_error(&VrcError::Timeout).contains("timed out"));
    i18n::set_lang("ja").unwrap();
}

// ---------- 認証 ----------

fn auth_server(kinds: &'static str, good_code: &'static str, verify_path: &'static str) -> Mock {
    let verified = Arc::new(AtomicBool::new(false));
    spawn(move |r| match (r.method.as_str(), path_of(r)) {
        ("GET", "/auth/user") if verified.load(Ordering::SeqCst) => Resp::json(r#"{"id":"usr_me"}"#),
        ("GET", "/auth/user") => Resp::json(&format!(r#"{{"requiresTwoFactorAuth":{kinds}}}"#)).cookie("auth=authcookie_first; Path=/; HttpOnly"),
        ("POST", p) if p == verify_path => {
            if r.header("cookie").map_or(false, |c| c.contains("auth=authcookie_first")) && r.body.contains(good_code) {
                verified.store(true, Ordering::SeqCst);
                Resp::json(r#"{"verified":true}"#).cookie("twoFactorAuth=tfa_cookie; Path=/")
            } else {
                Resp::status(400, r#"{"error":{"message":"bad code"}}"#)
            }
        }
        ("GET", "/auth") => Resp::json(r#"{"ok":true,"token":"authcookie_final"}"#),
        _ => Resp::status(404, "{}"),
    })
}

#[test]
fn login_with_totp_then_token_is_saved() {
    let m = auth_server(r#"["totp","otp"]"#, "123456", "/auth/twofactorauth/totp/verify");
    let file = tmp("totp");
    let asked = Arc::new(Mutex::new(vec![]));
    let a2 = asked.clone();
    let v = client(&m, &file).with_prompt(move |msg| {
        a2.lock().unwrap().push(msg.to_string());
        Some("123456\n".into())
    });
    v.login().unwrap();

    assert_eq!(asked.lock().unwrap().as_slice(), ["認証アプリの2FAコード: "]);
    assert_eq!(m.paths(), ["GET /auth/user", "POST /auth/twofactorauth/totp/verify", "GET /auth/user", "GET /auth"]);
    let reqs = m.requests();
    assert_eq!(reqs[0].header("authorization"), Some(BASIC)); // 最初はBasic認証
    assert_eq!(reqs[0].header("user-agent"), Some("TestBot/1.0 test@example.com"));
    assert_eq!(reqs[1].header("authorization"), None); // 2FA確認はクッキーだけ
    assert_eq!(reqs[1].header("cookie"), Some("auth=authcookie_first"));
    assert!(reqs[1].body.contains("\"code\":\"123456\""), "{}", reqs[1].body);
    assert_eq!(reqs[2].header("cookie"), Some("auth=authcookie_first; twoFactorAuth=tfa_cookie")); // 2FA後は両方のクッキー
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "authcookie_final");
    // 以後の呼び出しはクッキーつき
    assert_eq!(v.get_token().unwrap(), "authcookie_final");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[test]
fn login_with_email_otp_uses_email_endpoint() {
    let m = auth_server(r#"["emailOtp"]"#, "654321", "/auth/twofactorauth/emailotp/verify");
    let file = tmp("email");
    let asked = Arc::new(Mutex::new(vec![]));
    let a2 = asked.clone();
    let v = client(&m, &file).with_prompt(move |msg| {
        a2.lock().unwrap().push(msg.to_string());
        Some(" 654321 ".into()) // 前後の空白は取る
    });
    v.login().unwrap();
    assert_eq!(asked.lock().unwrap().as_slice(), ["メール2FAコード: "]);
    assert!(m.paths().contains(&"POST /auth/twofactorauth/emailotp/verify".to_string()));
}

#[test]
fn login_without_input_needs_reauth_and_wrong_code_fails() {
    let m = auth_server(r#"["totp"]"#, "123456", "/auth/twofactorauth/totp/verify");
    let e = client(&m, &tmp("eof")).login().unwrap_err(); // 入力できない(標準入力が閉じている)
    assert!(matches!(e, VrcError::NeedsReauth), "{e:?}");
    let e = client(&m, &tmp("bad")).with_prompt(|_| Some("000000".into())).login().unwrap_err();
    assert_eq!(e.status(), Some(400));
}

#[test]
fn login_reuses_saved_token() {
    let m = spawn(|r| match (path_of(r), r.header("cookie")) {
        ("/auth/user", Some("auth=authcookie_saved")) => Resp::json(r#"{"id":"usr_me"}"#),
        ("/auth", Some("auth=authcookie_saved")) => Resp::json(r#"{"ok":true,"token":"authcookie_saved"}"#),
        _ => Resp::status(401, "{}"),
    });
    let file = tmp("saved");
    std::fs::write(&file, "authcookie_saved\n").unwrap(); // Python版が書いた形式 (末尾の改行があっても読める)
    let v = client(&m, &file);
    v.login().unwrap();
    assert_eq!(m.paths(), ["GET /auth/user"]);
    assert_eq!(m.requests()[0].header("authorization"), None); // 保存済みトークンだけで通った
    assert_eq!(v.get_token().unwrap(), "authcookie_saved");
}

#[test]
fn login_falls_back_when_saved_token_is_stale() {
    let m = spawn(|r| match (path_of(r), r.header("cookie"), r.header("authorization")) {
        ("/auth/user", Some("auth=authcookie_old"), _) => Resp::status(401, "{}"),
        ("/auth/user", _, Some(_)) => Resp::json(r#"{"id":"usr_me"}"#).cookie("auth=authcookie_new; Path=/"),
        ("/auth", Some("auth=authcookie_new"), _) => Resp::json(r#"{"ok":true,"token":"authcookie_new"}"#),
        _ => Resp::status(401, "{}"),
    });
    let file = tmp("stale");
    std::fs::write(&file, "authcookie_old").unwrap();
    client(&m, &file).login().unwrap();
    let reqs = m.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[0].header("cookie"), Some("auth=authcookie_old"));
    assert_eq!(reqs[1].header("authorization"), Some(BASIC));
    assert_eq!(reqs[1].header("cookie"), None); // 古いクッキーは捨てている
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "authcookie_new");
}

#[test]
fn get_token_relogs_in_when_session_is_invalid() {
    let m = spawn(|r| match (path_of(r), r.header("cookie")) {
        ("/auth", Some("auth=tok1")) => Resp::json(r#"{"ok":true,"token":"tok1"}"#),
        ("/auth", _) => Resp::status(401, "{}"),
        ("/auth/user", _) => Resp::json(r#"{"id":"usr_me"}"#).cookie("auth=tok1; Path=/"),
        _ => Resp::status(404, "{}"),
    });
    let v = client(&m, &tmp("relogin"));
    assert_eq!(v.get_token().unwrap(), "tok1");
    assert_eq!(m.paths(), ["GET /auth", "GET /auth/user", "GET /auth"]);
}

// ---------- 参照系 ----------

#[test]
fn read_apis_map_fields() {
    let long_desc = "あ".repeat(300);
    let m = spawn(move |r| match r.path.as_str() {
        "/users/usr_abc" => Resp::json(r#"{"displayName":"Alice","status":"join me","statusDescription":"hi","isFriend":true,"state":"online"}"#),
        "/users/usr_x" => Resp::json(r#"{"displayName":"B","status":"active","statusDescription":null,"isFriend":false}"#),
        "/worlds/wrld_a" => Resp::json(&format!(r#"{{"name":"W","authorName":"AUTHOR","capacity":32,"thumbnailImageUrl":"https://t/x.png","description":"{long_desc}"}}"#)),
        "/instances/wrld_a:12345~hidden%28usr_y%29~region%28jp%29" => Resp::json(r#"{"n_users":5,"capacity":16}"#),
        "/instances/wrld_a:999" => Resp::json(r#"{"capacity":16}"#),
        "/groups/grp_y" => Resp::json(r#"{"name":"MyGroup"}"#),
        _ => Resp::status(404, r#"{"error":{"message":"nope"}}"#),
    });
    let v = client(&m, &tmp("read"));

    let u = v.get_user("usr_abc").unwrap();
    assert_eq!((u.name.as_str(), u.status.as_str(), u.desc.as_str(), u.is_friend, u.state.as_str()), ("Alice", "join me", "hi", true, "online"));
    let u = v.get_user("usr_x").unwrap();
    assert_eq!((u.desc.as_str(), u.is_friend, u.state.as_str()), ("", false, "")); // null は空文字に

    let w = v.get_world("wrld_a").unwrap();
    assert_eq!((w.name.as_str(), w.author.as_str(), w.capacity, w.thumb.as_deref()), ("W", "AUTHOR", Some(32), Some("https://t/x.png")));
    assert_eq!(w.desc.chars().count(), 200); // 説明は200文字まで

    let i = v.get_instance("wrld_a", "12345~hidden(usr_y)~region(jp)").unwrap();
    assert_eq!((i.n_users, i.capacity), (Some(5), Some(16)));
    let i = v.get_instance("wrld_a", "999").unwrap();
    assert_eq!((i.n_users, i.capacity), (None, Some(16)));
    assert_eq!(v.get_group_name("grp_y").unwrap(), "MyGroup");

    assert_eq!(v.get_user("usr_none").unwrap_err().status(), Some(404));
}

fn friend(id: &str, loc: Option<&str>) -> String {
    match loc {
        Some(l) => format!(r#"{{"id":"{id}","displayName":"N-{id}","status":"active","statusDescription":"d","location":"{l}"}}"#),
        None => format!(r#"{{"id":"{id}","displayName":"N-{id}","status":"busy"}}"#),
    }
}

#[test]
fn friends_are_paged_deduped_and_ingame_flagged() {
    let m = spawn(|r| {
        let q = r.path.split_once('?').map(|x| x.1).unwrap_or("");
        let page: String = match q {
            "offset=0&n=100&offline=false" => {
                let mut v = vec![friend("u0", Some("wrld_x:1~region(jp)")), friend("u1", Some("traveling")), friend("u2", Some("")), friend("u3", None)];
                v.extend((4..100).map(|i| friend(&format!("u{i}"), Some("offline"))));
                v.join(",")
            }
            "offset=100&n=100&offline=false" => (100..104).map(|i| friend(&format!("u{i}"), Some("private"))).chain([friend("u5", Some("private"))]).collect::<Vec<_>>().join(","), // u5 は重複
            "offset=0&n=100&offline=true" => [friend("u100", Some("offline")), friend("u200", Some("offline")), friend("u201", Some("offline"))].join(","), // u100 は重複
            _ => return Resp::status(400, "{}"),
        };
        Resp::json(&format!("[{page}]"))
    });
    let all = client(&m, &tmp("friends")).get_friends_all().unwrap();
    assert_eq!(all.len(), 100 + 4 + 2);
    assert_eq!(
        m.paths(),
        ["GET /auth/user/friends?offset=0&n=100&offline=false", "GET /auth/user/friends?offset=100&n=100&offline=false", "GET /auth/user/friends?offset=0&n=100&offline=true"]
    );
    let by = |id: &str| all.iter().find(|f| f.uid == id).unwrap().clone();
    assert!(by("u0").ingame && by("u0").location == "wrld_x:1~region(jp)");
    assert!(by("u1").ingame); // traveling もゲーム内扱い (offline 以外)
    assert!(!by("u2").ingame && by("u2").location == "offline"); // 空の location は offline
    assert!(!by("u3").ingame && by("u3").location == "offline"); // location なしも offline
    assert_eq!((by("u3").name.as_str(), by("u3").status.as_str(), by("u3").desc.as_str()), ("N-u3", "busy", ""));
    assert!(by("u100").ingame && by("u100").location == "private"); // 先に出たほうを採用
    assert_eq!(all.iter().filter(|f| f.uid == "u5").count(), 1);
    assert!(!by("u200").ingame);
}

// ---------- エラー ----------

#[test]
fn http_errors_and_timeouts_are_classified() {
    let m = spawn(|r| match path_of(r) {
        "/users/a" => Resp::status(429, "{}"),
        "/users/b" => Resp::status(503, "{}"),
        "/users/c" => Resp::status(401, "{}"),
        "/users/slow" => Resp::json("{}").delay(1500),
        "/users/garbage" => Resp::json("not json"),
        _ => Resp::json("{}"),
    });
    let v = client(&m, &tmp("err")).with_timeout(Duration::from_millis(400));
    assert_eq!(v.get_user("a").unwrap_err().status(), Some(429));
    assert_eq!(v.get_user("b").unwrap_err().status(), Some(503));
    assert_eq!(v.get_user("c").unwrap_err().status(), Some(401));
    assert!(matches!(v.get_user("slow").unwrap_err(), VrcError::Timeout));
    assert!(matches!(v.get_user("garbage").unwrap_err(), VrcError::Parse(_)));
    let dead = Vrc::new("u", "p", "ua").with_base("http://127.0.0.1:1").with_timeout(Duration::from_millis(400));
    assert!(matches!(dead.get_user("a").unwrap_err(), VrcError::Transport(_))); // 接続できない
}
