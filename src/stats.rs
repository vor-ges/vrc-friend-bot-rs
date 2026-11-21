//! stats.py の移植: ログイン統計。ゲーム内ログイン/ログアウト/ステータス変更を履歴として保存し、集計する。
//!
//! 既存の friends テーブルには触れず、専用テーブル(activity / stats_meta / vrc_friends)を IF NOT EXISTS で追加するだけ。
//! vrc_friends = VRChat上の全フレンドの名簿。Discordに通知する登録(friends)とは独立で、統計は名簿の全員分を記録する。
//! kind: login=ゲーム内ログイン(頻度に数える) / resume=ログイン時刻不明の復元(滞在時間のみ数える)
//!       status=ステータス変更 / logout=ゲームから離脱
use crate::db::{lock, now, Conn};
use chrono::{TimeZone, Timelike};
use chrono_tz::Tz;
use rusqlite::{params, params_from_iter, types::Value, OptionalExtension, Row};
use std::collections::{HashMap, HashSet};

/// 名前 → 秒。Python の dict と同じく**初めて現れた順**を保つ(同率のときの表示順が揺れないように)。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusMap(Vec<(String, f64)>);

impl StatusMap {
    pub fn add(&mut self, k: &str, v: f64) {
        match self.0.iter_mut().find(|(n, _)| n == k) {
            Some(e) => e.1 += v,
            None => self.0.push((k.to_string(), v)),
        }
    }
    pub fn get(&self, k: &str) -> Option<f64> {
        self.0.iter().find(|(n, _)| n == k).map(|(_, v)| *v)
    }
    pub fn contains(&self, k: &str) -> bool {
        self.get(k).is_some()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, f64)> {
        self.0.iter().map(|(k, v)| (k.as_str(), *v))
    }
    pub fn total(&self) -> f64 {
        self.0.iter().map(|(_, v)| v).sum()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// stat = {logins, hours[24](ログイン時間帯別回数), status{名前: 秒}, seconds(ゲーム内滞在秒)}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stat {
    pub logins: u32,
    pub hours: [u32; 24],
    pub status: StatusMap,
    pub seconds: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub per: HashMap<String, Stat>,
    pub total: Stat,
}

/// vrc_friends の1行。
#[derive(Clone, Debug, PartialEq)]
pub struct VrcFriend {
    pub uid: String,
    pub name: Option<String>,
    pub is_friend: bool,
    pub tracked_since: f64,
    pub updated_at: Option<f64>,
}

impl VrcFriend {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(VrcFriend {
            uid: r.get("uid")?,
            name: r.get("name")?,
            is_friend: r.get::<_, i64>("is_friend")? != 0,
            tracked_since: r.get("tracked_since")?,
            updated_at: r.get("updated_at")?,
        })
    }
}

/// タイムゾーン名を読む。読めなければ Asia/Tokyo (Python版の UTC+9 フォールバックと同じ)。
/// tzdata は chrono-tz に同梱されるので、OS側にタイムゾーンDBが無くても読める。
pub fn make_tz(name: &str) -> Tz {
    name.parse::<Tz>().unwrap_or_else(|_| {
        log::warn!("timezone {name:?} を読み込めないため UTC+9 を使います");
        chrono_tz::Asia::Tokyo
    })
}

pub struct Stats {
    c: Conn,
    tz: Tz,
    /// 前回プロセスが最後に生きていた時刻 (起動時の照合用)
    pub prev_alive: Option<f64>,
}

#[derive(Default)]
struct State {
    ing: bool,
    cur: String,
    t0: f64,
}

fn close(acc: &mut Stat, st: &State, start: f64, end: f64, t1: f64) {
    let (a, b) = (st.t0.max(start), t1.min(end));
    if st.ing && b > a {
        acc.status.add(&st.cur, b - a);
        acc.seconds += b - a;
    }
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

impl Stats {
    pub fn new(c: Conn, tz: Tz) -> rusqlite::Result<Self> {
        {
            let conn = lock(&c);
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS activity(
                    id INTEGER PRIMARY KEY AUTOINCREMENT, uid TEXT NOT NULL,
                    ts REAL NOT NULL, kind TEXT NOT NULL, status TEXT);
                 CREATE INDEX IF NOT EXISTS idx_activity_uid_ts ON activity(uid, ts);
                 CREATE TABLE IF NOT EXISTS stats_meta(k TEXT PRIMARY KEY, v TEXT);
                 CREATE TABLE IF NOT EXISTS vrc_friends(
                    uid TEXT PRIMARY KEY, name TEXT, is_friend INTEGER NOT NULL DEFAULT 1,
                    tracked_since REAL NOT NULL, updated_at REAL);",
            )?;
            conn.execute("INSERT OR IGNORE INTO stats_meta(k,v) VALUES('since', ?)", [now().to_string()])?;
        }
        let mut s = Stats { c, tz, prev_alive: None };
        s.prev_alive = s.meta("alive")?;
        Ok(s)
    }

    pub fn tz(&self) -> Tz {
        self.tz
    }

    /// 共有接続 (Python版の `Stats.c`)。テストやメンテナンス用。
    pub fn conn(&self) -> Conn {
        self.c.clone()
    }

    // ---------- meta ----------
    fn meta(&self, k: &str) -> rusqlite::Result<Option<f64>> {
        let v: Option<String> =
            lock(&self.c).query_row("SELECT v FROM stats_meta WHERE k=?", [k], |r| r.get(0)).optional()?;
        Ok(v.and_then(|v| v.parse().ok()))
    }

    pub fn since(&self) -> rusqlite::Result<Option<f64>> {
        self.meta("since")
    }

    pub fn heartbeat(&self) {
        let r = lock(&self.c).execute(
            "INSERT INTO stats_meta(k,v) VALUES('alive',?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            [now().to_string()],
        );
        if let Err(e) = r {
            log::error!("stats heartbeat failed: {e}");
        }
    }

    // ---------- フレンド名簿 (VRChat上の全フレンド) ----------
    /// 名簿に追加/更新(フレンドである印も付け直す)。新規ならtrueを返す。
    /// 新規の収集開始日時: 既に履歴があればその最初の時刻まで遡る。
    /// hint(Discord登録日時)があれば統計開始以降でそれを使う。
    pub fn upsert_friend(&self, uid: &str, name: Option<&str>, hint: Option<f64>) -> rusqlite::Result<bool> {
        let t = now();
        let name = name.filter(|n| !n.is_empty());
        let since = self.since()?;
        let c = lock(&self.c);
        let exists = c.query_row("SELECT 1 FROM vrc_friends WHERE uid=?", [uid], |_| Ok(())).optional()?.is_some();
        if exists {
            c.execute(
                "UPDATE vrc_friends SET name=COALESCE(?,name), is_friend=1, updated_at=? WHERE uid=?",
                params![name, t, uid],
            )?;
            return Ok(false);
        }
        let mut base = t;
        if let Some(h) = hint.filter(|h| *h != 0.0) {
            base = t.min(h.max(since.filter(|s| *s != 0.0).unwrap_or(h)));
        }
        let first: Option<f64> =
            c.query_row("SELECT MIN(ts) FROM activity WHERE uid=?", [uid], |r| r.get(0))?;
        if let Some(f) = first {
            base = base.min(f);
        }
        c.execute(
            "INSERT INTO vrc_friends(uid,name,is_friend,tracked_since,updated_at) VALUES(?,?,1,?,?)",
            params![uid, name, base, t],
        )?;
        Ok(true)
    }

    pub fn mark_unfriended(&self, uid: &str) -> rusqlite::Result<()> {
        lock(&self.c).execute("UPDATE vrc_friends SET is_friend=0, updated_at=? WHERE uid=?", params![now(), uid])?;
        Ok(())
    }

    pub fn friend(&self, uid: &str) -> rusqlite::Result<Option<VrcFriend>> {
        lock(&self.c).query_row("SELECT * FROM vrc_friends WHERE uid=?", [uid], VrcFriend::from_row).optional()
    }

    /// 名簿の全員(フレンド解除済みも含む。履歴は残るため)。表示名順。
    pub fn friends(&self) -> rusqlite::Result<Vec<VrcFriend>> {
        let c = lock(&self.c);
        let mut st = c.prepare_cached("SELECT * FROM vrc_friends ORDER BY LOWER(COALESCE(name, uid)), uid")?;
        let rows = st.query_map([], VrcFriend::from_row)?;
        rows.collect()
    }

    /// 名簿にフレンドとして載っているが、最新の一覧にいない人
    pub fn missing<S: AsRef<str>>(&self, present_uids: &[S]) -> rusqlite::Result<Vec<String>> {
        let present: HashSet<&str> = present_uids.iter().map(|s| s.as_ref()).collect();
        let c = lock(&self.c);
        let mut st = c.prepare_cached("SELECT uid FROM vrc_friends WHERE is_friend=1")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = vec![];
        for u in rows {
            let u = u?;
            if !present.contains(u.as_str()) {
                out.push(u);
            }
        }
        Ok(out)
    }

    /// uids の収集開始日時の最小値。名簿にまだ無い人は統計開始(since)として扱う。
    pub fn tracked_since<S: AsRef<str>>(&self, uids: &[S]) -> rusqlite::Result<Option<f64>> {
        let since = self.since()?;
        if uids.is_empty() {
            return Ok(since);
        }
        let sql = format!("SELECT uid, tracked_since FROM vrc_friends WHERE uid IN ({})", placeholders(uids.len()));
        let mut have: HashMap<String, f64> = HashMap::new();
        {
            let c = lock(&self.c);
            let mut st = c.prepare(&sql)?;
            let rows = st.query_map(params_from_iter(uids.iter().map(|u| u.as_ref())), |r| Ok((r.get(0)?, r.get(1)?)))?;
            for r in rows {
                let (u, t): (String, f64) = r?;
                have.insert(u, t);
            }
        }
        Ok(uids
            .iter()
            .filter_map(|u| have.get(u.as_ref()).copied().or(since))
            .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.min(v)))))
    }

    // ---------- 記録 ----------
    /// 履歴を1行追記する。失敗しても通知処理は止めない(ログだけ残す)。
    pub fn record(&self, uid: &str, kind: &str, status: Option<&str>, ts: Option<f64>) {
        let r = lock(&self.c).execute(
            "INSERT INTO activity(uid,ts,kind,status) VALUES(?,?,?,?)",
            params![uid, ts.unwrap_or_else(now), kind, status],
        );
        if let Err(e) = r {
            log::error!("stats record failed uid={uid} kind={kind}: {e}");
        }
    }

    /// (履歴上ゲーム内か, 直近のステータス, 最終イベント時刻)
    pub fn state(&self, uid: &str) -> rusqlite::Result<(bool, Option<String>, Option<f64>)> {
        let r: Option<(String, Option<String>, f64)> = lock(&self.c)
            .query_row(
                "SELECT kind,status,ts FROM activity WHERE uid=? ORDER BY ts DESC, id DESC LIMIT 1",
                [uid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(match r {
            None => (false, None, None),
            Some((kind, status, ts)) => (kind != "logout", status, Some(ts)),
        })
    }

    /// 履歴と現在の実状態(online_now)を突き合わせて、停止中などで生じたズレを補正する。
    pub fn reconcile(&self, uid: &str, online_now: bool, status: Option<&str>, close_at: Option<f64>) -> rusqlite::Result<()> {
        let (logged, last_status, last_ts) = self.state(uid)?;
        if online_now && !logged {
            self.record(uid, "resume", status, None);
        } else if !online_now && logged {
            let t = now();
            let ts = match close_at {
                None => t,
                Some(c) => t.min(last_ts.unwrap_or(c).max(c)),
            };
            self.record(uid, "logout", None, Some(ts));
        } else if online_now && logged {
            if let Some(s) = status.filter(|s| !s.is_empty()) {
                if Some(s) != last_status.as_deref() {
                    self.record(uid, "status", Some(s), None);
                }
            }
        }
        Ok(())
    }

    // ---------- 集計 ----------
    /// uids の [start, end] を集計する。
    pub fn summarize<S: AsRef<str>>(&self, uids: &[S], start: f64, end: f64) -> rusqlite::Result<Summary> {
        let mut per: HashMap<String, Stat> = HashMap::new();
        let mut order: Vec<String> = vec![];
        for u in uids {
            if !per.contains_key(u.as_ref()) {
                per.insert(u.as_ref().to_string(), Stat::default());
                order.push(u.as_ref().to_string());
            }
        }
        if !order.is_empty() {
            let sql = format!(
                "SELECT uid,ts,kind,status FROM activity WHERE uid IN ({}) AND ts<=? ORDER BY uid, ts, id",
                placeholders(order.len())
            );
            let mut args: Vec<Value> = order.iter().map(|u| Value::Text(u.clone())).collect();
            args.push(Value::Real(end));

            let c = lock(&self.c);
            let mut stmt = c.prepare(&sql)?;
            let mut rows = stmt.query(params_from_iter(args.iter()))?;

            // uid順に並んでいるので、1人分ずつ状態を持ち回して区切りで確定する。
            let mut cur_uid = String::new();
            let mut have = false;
            let mut acc = Stat::default();
            let mut st = State::default();
            while let Some(r) = rows.next()? {
                let uid = r.get_ref(0)?.as_str()?;
                let ts: f64 = r.get(1)?;
                let kind = r.get_ref(2)?.as_str()?;
                let status = r.get_ref(3)?.as_str_or_null()?.filter(|s| !s.is_empty()).unwrap_or("unknown");

                if !have || uid != cur_uid {
                    if have {
                        if st.ing {
                            close(&mut acc, &st, start, end, end); // まだゲーム内なら現在まで
                        }
                        per.insert(std::mem::take(&mut cur_uid), std::mem::take(&mut acc));
                    }
                    cur_uid = uid.to_string();
                    have = true;
                    st = State { ing: false, cur: String::new(), t0: ts };
                }
                match kind {
                    "login" | "resume" => {
                        if st.ing {
                            close(&mut acc, &st, start, end, ts); // 二重ログイン: 前の区間をここで閉じる
                        }
                        st.ing = true;
                        st.cur.clear();
                        st.cur.push_str(status);
                        st.t0 = ts;
                        if kind == "login" && start <= ts && ts <= end {
                            acc.logins += 1;
                            if let Some(dt) = self.tz.timestamp_opt(ts.floor() as i64, 0).single() {
                                acc.hours[dt.hour() as usize] += 1;
                            }
                        }
                    }
                    "status" => {
                        if st.ing {
                            close(&mut acc, &st, start, end, ts);
                            st.cur.clear();
                            st.cur.push_str(status);
                            st.t0 = ts;
                        }
                    }
                    "logout" => {
                        if st.ing {
                            close(&mut acc, &st, start, end, ts);
                            st.ing = false;
                            st.cur.clear();
                            st.t0 = ts;
                        }
                    }
                    _ => {}
                }
            }
            if have {
                if st.ing {
                    close(&mut acc, &st, start, end, end);
                }
                per.insert(cur_uid, acc);
            }
        }

        let mut total = Stat::default();
        for u in &order {
            let p = &per[u];
            total.logins += p.logins;
            total.seconds += p.seconds;
            for h in 0..24 {
                total.hours[h] += p.hours[h];
            }
            for (k, v) in p.status.iter() {
                total.status.add(k, v);
            }
        }
        Ok(Summary { per, total })
    }
}
