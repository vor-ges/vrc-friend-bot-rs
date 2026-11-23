//! db.py の移植: Discordに通知する登録フレンド(friends テーブル)。
use rusqlite::{params, Connection, Row};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Python版の `DB.c` に当たる共有接続。stats / i18n も同じ接続を使う。
pub type Conn = Arc<Mutex<Connection>>;

/// 接続のロック。別スレッドがpanicしてMutexが汚染されても通知処理は止めない。
pub fn lock(c: &Conn) -> MutexGuard<'_, Connection> {
    c.lock().unwrap_or_else(|e| e.into_inner())
}

/// time.time() 相当 (UNIX秒, 小数あり)。
pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Friend {
    pub uid: String,
    pub name: Option<String>,
    pub channel_id: Option<i64>,
    pub active: bool,
    pub added_at: Option<f64>,
}

impl Friend {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Friend {
            uid: r.get("uid")?,
            name: r.get("name")?,
            channel_id: r.get("channel_id")?,
            active: r.get::<_, Option<i64>>("active")?.unwrap_or(0) != 0,
            added_at: r.get("added_at")?,
        })
    }
}

/// 接続を開く。`query_only` なら、書き込みをDB側で禁止する (PRAGMA query_only)。
/// 他のプロセス(Python版Bot)が書いているDBを読むだけで使うときに使う。
pub fn open_conn(path: &str, query_only: bool) -> rusqlite::Result<Conn> {
    let conn = if path == ":memory:" { Connection::open_in_memory()? } else { Connection::open(path)? };
    conn.busy_timeout(std::time::Duration::from_secs(10))?; // 他のプロセスの書き込み中は待つ
    if query_only {
        conn.pragma_update(None, "query_only", true)?;
    }
    Ok(Arc::new(Mutex::new(conn)))
}

/// 書き込みと読み込みが同時にできるモード(WAL)にする。DBファイルに永続する設定。
pub fn enable_wal(c: &Conn) -> rusqlite::Result<String> {
    lock(c).query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
}

pub struct Db {
    pub c: Conn,
}

impl Db {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = if path == ":memory:" { Connection::open_in_memory()? } else { Connection::open(path)? };
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS friends(
                uid TEXT PRIMARY KEY, name TEXT, channel_id INTEGER,
                active INTEGER DEFAULT 1, added_at REAL)",
        )?;
        Ok(Db { c: Arc::new(Mutex::new(conn)) })
    }

    /// 他のBotの登録を引き継ぐときに使う。登録日時(added_at)も元のまま入れる。
    pub fn import(&self, uid: &str, name: Option<&str>, channel_id: i64, active: bool, added_at: Option<f64>) -> rusqlite::Result<()> {
        lock(&self.c).execute(
            "INSERT INTO friends(uid,name,channel_id,active,added_at) VALUES(?,?,?,?,?) ON CONFLICT(uid) DO UPDATE SET
             name=excluded.name, channel_id=excluded.channel_id, active=excluded.active",
            params![uid, name, channel_id, active as i64, added_at.unwrap_or_else(now)],
        )?;
        Ok(())
    }

    pub fn memory() -> rusqlite::Result<Self> {
        Self::open(":memory:")
    }

    pub fn get(&self, uid: &str) -> rusqlite::Result<Option<Friend>> {
        let c = lock(&self.c);
        let mut st = c.prepare_cached("SELECT * FROM friends WHERE uid=?")?;
        let mut rows = st.query_map(params![uid], Friend::from_row)?;
        rows.next().transpose()
    }

    pub fn upsert(&self, uid: &str, name: &str, channel_id: i64) -> rusqlite::Result<()> {
        lock(&self.c).execute(
            "INSERT INTO friends(uid,name,channel_id,active,added_at)
             VALUES(?,?,?,1,?) ON CONFLICT(uid) DO UPDATE SET
             name=excluded.name, channel_id=excluded.channel_id, active=1",
            params![uid, name, channel_id, now()],
        )?;
        Ok(())
    }

    pub fn deactivate(&self, uid: &str) -> rusqlite::Result<()> {
        lock(&self.c).execute("UPDATE friends SET active=0 WHERE uid=?", params![uid])?;
        Ok(())
    }

    pub fn active(&self) -> rusqlite::Result<Vec<Friend>> {
        let c = lock(&self.c);
        let mut st = c.prepare_cached("SELECT * FROM friends WHERE active=1 ORDER BY name")?;
        let rows = st.query_map([], Friend::from_row)?;
        rows.collect()
    }

    pub fn all(&self) -> rusqlite::Result<Vec<Friend>> {
        let c = lock(&self.c);
        let mut st = c.prepare_cached("SELECT * FROM friends ORDER BY active DESC, name")?;
        let rows = st.query_map([], Friend::from_row)?;
        rows.collect()
    }
}
