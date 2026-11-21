//! 本物のVRChatで vrc.rs を確かめる確認用プログラム (モックでは確認できない部分の実機チェック)。
//!
//!   VRC_USERNAME=... VRC_PASSWORD=... VRC_USER_AGENT="VRCFriendBot/1.0 you@example.com" \
//!     cargo run --release --example live_check [usr_xxxx...]
//!
//! やること: ログイン(2FAは対話入力) → トークン取得 → フレンド一覧(件数とゲーム内人数) → 引数のUIDの取得。
//! パスワードやトークンの中身は表示しない。カレントディレクトリの .vrc_auth を Python版と共有する。
use vrc_friend_bot_rs::vrc::{explain_error, Vrc};

fn main() {
    let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("環境変数 {k} が必要です"));
    let ua = std::env::var("VRC_USER_AGENT").unwrap_or_else(|_| "VRCFriendBot/1.0".into());
    let v = Vrc::new(&env("VRC_USERNAME"), &env("VRC_PASSWORD"), &ua);

    let step = |name: &str, r: Result<String, vrc_friend_bot_rs::vrc::VrcError>| match r {
        Ok(msg) => println!("[ OK ] {name}: {msg}"),
        Err(e) => {
            println!("[FAIL] {name}: {e} / 表示文: {}", explain_error(&e));
            std::process::exit(1);
        }
    };

    step("login", v.login().map(|_| "ログインできた".into()));
    step("get_token", v.get_token().map(|t| format!("トークン取得 (長さ {})", t.len())));
    step(
        "get_friends_all",
        v.get_friends_all().map(|f| {
            let ing = f.iter().filter(|x| x.ingame).count();
            let mut s = format!("{}人 (ゲーム内 {ing}人)", f.len());
            if let Some(x) = f.iter().find(|x| x.ingame).or(f.first()) {
                s += &format!(" / 例: {} status={} location={}", x.name, x.status, x.location);
            }
            s
        }),
    );
    for uid in std::env::args().skip(1) {
        step(&format!("get_user {uid}"), v.get_user(&uid).map(|u| format!("{} status={} friend={} state={}", u.name, u.status, u.is_friend, u.state)));
    }
}
