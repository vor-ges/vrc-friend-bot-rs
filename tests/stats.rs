//! test_local.py の「統計 (/stats)」「全フレンドの収集 (vrc_friends)」の該当部分の移植。
//! 期待値はPython版と同じ。
use chrono::{Duration, TimeZone};
use chrono_tz::Asia::Tokyo;
use vrc_friend_bot_rs::db::{lock, now, Db};
use vrc_friend_bot_rs::stats::{make_tz, Stats};

/// BASE = 10日前の0:00(JST)。T(d,h,m) = BASE + (d-1)日 + h時間 + m分 のUNIX秒。
fn t(d: i64, h: i64, m: i64) -> f64 {
    let today = Tokyo.timestamp_opt(now() as i64, 0).unwrap().date_naive();
    let base = Tokyo.from_local_datetime(&today.and_hms_opt(0, 0, 0).unwrap()).unwrap() - Duration::days(10);
    (base + Duration::days(d - 1) + Duration::hours(h) + Duration::minutes(m)).timestamp() as f64
}

fn mkstats() -> Stats {
    let d = Db::memory().unwrap();
    let s = Stats::new(d.c.clone(), Tokyo).unwrap();
    lock(&d.c).execute("UPDATE stats_meta SET v=? WHERE k='since'", [t(1, 0, 0).to_string()]).unwrap();
    // Dbを手放しても接続はArcで生き続ける
    s
}

fn r(s: &Stats, uid: &str, kind: &str, status: Option<&str>, ts: f64) {
    s.record(uid, kind, status, Some(ts));
}

#[test]
fn summarize_basic_and_window() {
    let s = mkstats();
    r(&s, "A", "login", Some("join me"), t(1, 21, 0));
    r(&s, "A", "status", Some("busy"), t(1, 22, 0));
    r(&s, "A", "logout", None, t(1, 23, 30));
    r(&s, "B", "login", Some("active"), t(1, 21, 30));
    r(&s, "B", "logout", None, t(1, 22, 0));
    r(&s, "B", "login", Some("active"), t(2, 3, 0));
    r(&s, "B", "logout", None, t(2, 4, 0));
    let res = s.summarize(&["A", "B"], t(1, 0, 0), t(3, 0, 0)).unwrap();
    let (a, b, tot) = (&res.per["A"], &res.per["B"], &res.total);
    assert_eq!(a.status.get("join me"), Some(3600.0));
    assert_eq!(a.status.get("busy"), Some(5400.0));
    assert_eq!(a.logins, 1);
    assert_eq!(b.status.get("active"), Some(5400.0));
    assert_eq!(b.logins, 2);
    assert_eq!(tot.logins, 3);
    assert_eq!(tot.hours[21], 2);
    assert_eq!(tot.hours[3], 1);
    assert_eq!(tot.hours.iter().sum::<u32>(), 3); // 時間帯はJST
    assert_eq!(tot.seconds, 14400.0);

    // 期間の切り出し: 22:30以降だけ -> 21時のログインは数えず、滞在はbusyの1時間のみ
    let w = s.summarize(&["A"], t(1, 22, 30), t(3, 0, 0)).unwrap();
    assert_eq!(w.total.logins, 0);
    assert_eq!(w.total.status.get("busy"), Some(3600.0));
    assert!(!w.total.status.contains("join me"));
}

#[test]
fn summarize_edge_cases() {
    let s = mkstats();
    // resumeは頻度に数えないが滞在には数える / 継続中は終了時刻まで / 二重ログイン / ゲーム外のステータス変更は無視
    r(&s, "C", "resume", Some("busy"), t(1, 20, 0));
    r(&s, "C", "logout", None, t(1, 21, 0));
    r(&s, "D", "login", Some("ask me"), t(1, 20, 0));
    r(&s, "E", "login", Some("busy"), t(1, 20, 0));
    r(&s, "E", "login", Some("active"), t(1, 21, 0));
    r(&s, "E", "logout", None, t(1, 22, 0));
    r(&s, "F", "status", Some("busy"), t(1, 19, 0));
    r(&s, "F", "login", Some("active"), t(1, 20, 0));
    r(&s, "F", "logout", None, t(1, 21, 0));
    r(&s, "F", "status", Some("busy"), t(1, 22, 0));
    let p = s.summarize(&["C", "D", "E", "F"], t(1, 0, 0), t(1, 22, 0)).unwrap().per;
    assert_eq!(p["C"].logins, 0);
    assert_eq!(p["C"].status.get("busy"), Some(3600.0));
    assert_eq!(p["D"].status.get("ask me"), Some(7200.0));
    assert_eq!(p["E"].logins, 2);
    assert_eq!(p["E"].status.get("busy"), Some(3600.0));
    assert_eq!(p["E"].status.get("active"), Some(3600.0));
    assert_eq!(p["F"].status.get("active"), Some(3600.0));
    assert!(!p["F"].status.contains("busy"));
    let empty: [&str; 0] = [];
    assert_eq!(s.summarize(&empty, t(1, 0, 0), t(2, 0, 0)).unwrap().total.logins, 0);
}

#[test]
fn status_ties_keep_first_seen_order() {
    // Pythonのdictと同じく、同率のときは「初めて現れた順」になる (表示順が揺れない)
    let s = mkstats();
    r(&s, "X", "login", Some("busy"), t(1, 10, 0));
    r(&s, "X", "status", Some("active"), t(1, 11, 0));
    r(&s, "X", "status", Some("join me"), t(1, 12, 0));
    r(&s, "X", "logout", None, t(1, 13, 0));
    let names: Vec<_> = s.summarize(&["X"], t(1, 0, 0), t(2, 0, 0)).unwrap().total.status.iter().map(|(k, _)| k.to_string()).collect();
    assert_eq!(names, ["busy", "active", "join me"]);
}

#[test]
fn reconcile() {
    let s = mkstats();
    let count = |s: &Stats| -> i64 { lock(&s.conn()).query_row("SELECT COUNT(*) FROM activity", [], |r| r.get(0)).unwrap() };
    s.reconcile("G", true, Some("busy"), None).unwrap(); // 履歴なし+ゲーム内 -> resume
    assert_eq!(s.state("G").unwrap().0, true);
    assert_eq!(s.state("G").unwrap().1.as_deref(), Some("busy"));
    s.reconcile("G", true, Some("busy"), None).unwrap(); // 同状態は追記なし
    assert_eq!(count(&s), 1);
    s.reconcile("G", true, Some("active"), None).unwrap(); // ステータス変化を反映
    assert_eq!(s.state("G").unwrap().1.as_deref(), Some("active"));

    s.record("H", "login", Some("active"), Some(t(1, 20, 0)));
    s.reconcile("H", false, None, Some(t(1, 21, 30))).unwrap();
    assert_eq!(s.state("H").unwrap().0, false);
    assert_eq!(s.summarize(&["H"], t(1, 0, 0), t(2, 0, 0)).unwrap().total.seconds, 5400.0); // 停止直前の時刻で閉じる

    s.reconcile("I", false, None, None).unwrap(); // 何もなければ何もしない
    assert_eq!(s.state("I").unwrap(), (false, None, None));
}

#[test]
fn roster_and_tracked_since() {
    let s = mkstats();
    // 新規は今から
    assert!(s.upsert_friend("n1", Some("New"), None).unwrap());
    assert!((s.friend("n1").unwrap().unwrap().tracked_since - now()).abs() < 5.0);
    assert!(!s.upsert_friend("n1", Some("New2"), None).unwrap());
    assert_eq!(s.friend("n1").unwrap().unwrap().name.as_deref(), Some("New2"));
    assert!(!s.upsert_friend("n1", None, None).unwrap()); // Noneでは名前を消さない
    assert_eq!(s.friend("n1").unwrap().unwrap().name.as_deref(), Some("New2"));

    s.record("h1", "login", Some("active"), Some(t(3, 10, 0)));
    s.upsert_friend("h1", Some("Hist"), None).unwrap(); // 既存の履歴まで遡る
    assert_eq!(s.friend("h1").unwrap().unwrap().tracked_since, t(3, 10, 0));
    s.upsert_friend("h2", Some("A"), Some(t(1, 0, 0) - 86400.0)).unwrap(); // 登録日時が統計開始より前なら統計開始
    assert_eq!(s.friend("h2").unwrap().unwrap().tracked_since, t(1, 0, 0));
    s.upsert_friend("h3", Some("B"), Some(t(4, 0, 0))).unwrap(); // 後なら登録日時
    assert_eq!(s.friend("h3").unwrap().unwrap().tracked_since, t(4, 0, 0));
    s.record("h4", "login", Some("busy"), Some(t(3, 0, 0)));
    s.upsert_friend("h4", Some("C"), Some(t(5, 0, 0))).unwrap(); // 履歴と登録日時は早いほう
    assert_eq!(s.friend("h4").unwrap().unwrap().tracked_since, t(3, 0, 0));

    assert_eq!(s.tracked_since(&["h3", "h2"]).unwrap(), Some(t(1, 0, 0)));
    assert_eq!(s.tracked_since(&["h3"]).unwrap(), Some(t(4, 0, 0)));
    assert_eq!(s.tracked_since(&["zz"]).unwrap(), Some(t(1, 0, 0)));
    let none: [&str; 0] = [];
    assert_eq!(s.tracked_since(&none).unwrap(), Some(t(1, 0, 0)));

    s.mark_unfriended("h3").unwrap(); // 解除しても名簿(履歴)は残る
    assert!(!s.friend("h3").unwrap().unwrap().is_friend);
    assert!(s.friends().unwrap().iter().any(|f| f.uid == "h3"));
    let mut missing = s.missing(&["n1"]).unwrap();
    missing.sort();
    assert_eq!(missing, ["h1", "h2", "h4"]);
    assert!(!s.upsert_friend("h3", Some("B"), None).unwrap()); // 再びフレンドに
    assert!(s.friend("h3").unwrap().unwrap().is_friend);
}

#[test]
fn heartbeat_and_prev_alive() {
    let d = Db::memory().unwrap();
    let s = Stats::new(d.c.clone(), make_tz("Asia/Tokyo")).unwrap();
    assert_eq!(s.prev_alive, None);
    s.heartbeat();
    let s2 = Stats::new(d.c.clone(), make_tz("Asia/Tokyo")).unwrap(); // 再起動
    assert!((s2.prev_alive.unwrap() - now()).abs() < 5.0);
    assert_eq!(make_tz("No/Such_Zone"), Tokyo); // 読めなければ UTC+9
}

// ---------- 他のBot(Python版)が記録しているDBを、読み取り専用で参照する ----------

fn temp_db(name: &str) -> String {
    let p = std::env::temp_dir().join(format!("vrcrs-{}-{}-{name}.db", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    p.to_string_lossy().to_string()
}

fn counts(path: &str) -> (i64, i64, i64, String) {
    let c = rusqlite::Connection::open(path).unwrap();
    let n = |t: &str| c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get::<_, i64>(0)).unwrap();
    let meta: String = c.query_row("SELECT group_concat(k||'='||v) FROM (SELECT k,v FROM stats_meta ORDER BY k)", [], |r| r.get(0)).unwrap();
    (n("activity"), n("vrc_friends"), n("stats_meta"), meta)
}

#[test]
fn readonly_stats_reads_but_never_writes() {
    let path = temp_db("ro");
    {
        // 記録担当のBot(Python版に相当)が作ったDB
        let c = vrc_friend_bot_rs::db::open_conn(&path, false).unwrap();
        let w = Stats::new(c, Tokyo).unwrap();
        w.upsert_friend("usr_a", Some("Alice"), None).unwrap();
        w.record("usr_a", "login", Some("busy"), Some(t(2, 20, 0)));
        w.record("usr_a", "logout", None, Some(t(2, 21, 0)));
        w.heartbeat();
    }
    let before = counts(&path);
    let s = Stats::new_readonly(vrc_friend_bot_rs::db::open_conn(&path, true).unwrap(), Tokyo).unwrap();
    assert!(s.is_readonly());
    // 読み取りは動く
    assert_eq!(s.friend("usr_a").unwrap().unwrap().name.as_deref(), Some("Alice"));
    assert_eq!(s.summarize(&["usr_a"], t(1, 0, 0), t(3, 0, 0)).unwrap().total.seconds, 3600.0);
    assert!(s.prev_alive.is_some());
    // 書き込み系は何もしない (エラーにもしない)
    s.record("usr_a", "login", Some("active"), Some(t(3, 1, 0)));
    s.heartbeat();
    assert!(!s.upsert_friend("usr_new", Some("New"), None).unwrap());
    s.mark_unfriended("usr_a").unwrap();
    s.reconcile("usr_a", true, Some("active"), None).unwrap();
    assert!(s.friend("usr_new").unwrap().is_none() && s.friend("usr_a").unwrap().unwrap().is_friend);
    // それでも書こうとしたら、DB側が拒否する
    let cn = s.conn();
    let e = vrc_friend_bot_rs::db::lock(&cn).execute("DELETE FROM activity", []).unwrap_err();
    assert!(e.to_string().contains("readonly"), "{e}");
    drop(s);
    assert_eq!(counts(&path), before, "読み取り専用のあいだ、DBの中身は1行も変わらない");
}

#[test]
fn readonly_requires_existing_stats_tables() {
    let path = temp_db("empty");
    let c = vrc_friend_bot_rs::db::open_conn(&path, false).unwrap();
    vrc_friend_bot_rs::db::lock(&c).execute_batch("CREATE TABLE friends(uid TEXT)").unwrap(); // 統計テーブルが無いDB(パスの間違い)
    let e = Stats::new_readonly(c, Tokyo).err().unwrap();
    assert!(e.contains("STATS_DB"), "{e}");
}

#[test]
fn foreign_writer_is_detected_so_two_bots_do_not_both_record() {
    let path = temp_db("writer");
    let c = vrc_friend_bot_rs::db::open_conn(&path, false).unwrap();
    let s = Stats::new(c.clone(), Tokyo).unwrap();
    let set = |k: &str, v: f64| {
        vrc_friend_bot_rs::db::lock(&c).execute("INSERT INTO stats_meta(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v", rusqlite::params![k, v.to_string()]).unwrap();
    };
    assert!(!s.foreign_writer_active(180.0).unwrap()); // 誰も記録していない
    s.heartbeat(); // 自分(Rust版)が記録している -> 他のBotではない
    assert!(!s.foreign_writer_active(180.0).unwrap());
    set("alive", now()); // Python版の生存記録 (rust_alive は書かない)
    set("rust_alive", now() - 100.0);
    assert!(s.foreign_writer_active(180.0).unwrap()); // 他のBotが記録中
    set("alive", now() - 1000.0);
    assert!(!s.foreign_writer_active(180.0).unwrap()); // 古い記録は無視 (Python版は止まっている)
}
