//! 他のBot(Python版など)の登録フレンドを、このBotの登録に取り込む / 登録チャンネルの実在を確認する。
//! 取り込み元のDBは読み取り専用で開き、書き込まない。
use crate::bot::{Discord, DiscordError};
use crate::db::{lock, Db, Friend};
use crate::logic::slug;
use rusqlite::{Connection, OpenFlags};

#[derive(Debug, Clone, PartialEq)]
pub struct PyFriend {
    pub uid: String,
    pub name: Option<String>,
    pub channel_id: Option<i64>,
    pub active: bool,
    pub added_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// このBotにもう登録済み (何もしない)
    AlreadyRegistered,
    /// 削除済みの登録 (取り込まない)
    SkipRemoved,
    /// 新しいチャンネルを作って登録する
    Create,
    /// 元のBotのチャンネルIDをそのまま引き継ぐ (2つのBotが同じチャンネルに書き込むので、元のBotを止めてから)
    Reuse(i64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanItem {
    pub friend: PyFriend,
    pub action: Action,
}

#[derive(Debug)]
pub struct Outcome {
    pub uid: String,
    pub name: String,
    /// 登録したチャンネルID、または失敗の理由
    pub result: Result<i64, String>,
}

/// 他のBotのDBを、書き込み禁止で開く (存在しなければエラー。空のDBを作ってしまわない)。
fn open_readonly(path: &str) -> Result<Connection, String> {
    if !std::path::Path::new(path).is_file() {
        return Err(format!("ファイルが見つかりません: {path}"));
    }
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|e| format!("{path} を開けません: {e}"))?; // CREATEは付けない
    c.busy_timeout(std::time::Duration::from_secs(10)).map_err(|e| e.to_string())?;
    c.pragma_update(None, "query_only", true).map_err(|e| e.to_string())?; // WALのDBも読めるよう通常に開き、書き込みだけを禁止
    Ok(c)
}

pub fn read_python_friends(path: &str) -> Result<Vec<PyFriend>, String> {
    let c = open_readonly(path)?;
    let mut st = c
        .prepare("SELECT uid,name,channel_id,active,added_at FROM friends ORDER BY name")
        .map_err(|_| format!("{path} に friends テーブルがありません。Python版の bot.db ですか?"))?;
    let rows = st
        .query_map([], |r| {
            Ok(PyFriend { uid: r.get(0)?, name: r.get(1)?, channel_id: r.get(2)?, active: r.get::<_, Option<i64>>(3)?.unwrap_or(1) != 0, added_at: r.get(4)? })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e: rusqlite::Error| e.to_string())
}

/// 言語とメンションの設定 (lang / mention / mention:<uid>)。テーブルが無ければ空。
pub fn read_python_settings(path: &str) -> Result<Vec<(String, String)>, String> {
    let c = open_readonly(path)?;
    let Ok(mut st) = c.prepare("SELECT k,v FROM bot_settings WHERE k='lang' OR k='mention' OR k LIKE 'mention:%'") else { return Ok(vec![]) };
    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))).map_err(|e| e.to_string())?;
    Ok(rows.flatten().filter_map(|(k, v)| v.map(|v| (k, v))).collect())
}

pub fn plan(py: &[PyFriend], bot: &Db, reuse: bool, include_removed: bool) -> Vec<PlanItem> {
    py.iter()
        .map(|f| {
            let registered = bot.get(&f.uid).ok().flatten().map_or(false, |r| r.active);
            let action = if registered {
                Action::AlreadyRegistered
            } else if !f.active && !include_removed {
                Action::SkipRemoved
            } else {
                match (reuse, f.channel_id) {
                    (true, Some(ch)) => Action::Reuse(ch),
                    _ => Action::Create,
                }
            };
            PlanItem { friend: f.clone(), action }
        })
        .collect()
}

/// 計画を実行する。取り込めなかったものは理由つきで返す (残りは続ける)。
pub fn apply(items: &[PlanItem], bot: &Db, discord: &dyn Discord) -> Vec<Outcome> {
    let mut out = vec![];
    for it in items {
        let f = &it.friend;
        let name = f.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| f.uid.clone());
        let channel: Result<i64, String> = match &it.action {
            Action::AlreadyRegistered | Action::SkipRemoved => continue,
            Action::Create => discord.friend_channel(None, &slug(&name, &f.uid), &f.uid).map_err(|e| e.to_string()),
            Action::Reuse(ch) => match discord.channel_exists(*ch) {
                Ok(true) => Ok(*ch),
                Ok(false) => Err(format!("チャンネル {ch} が存在しない、またはBotから見えません")),
                Err(e) => Err(e.to_string()),
            },
        };
        let result = channel.and_then(|ch| bot.import(&f.uid, Some(&name), ch, f.active, f.added_at).map(|_| ch).map_err(|e| e.to_string()));
        out.push(Outcome { uid: f.uid.clone(), name, result });
    }
    out
}

/// 設定のうち、このBotにまだ無いものだけをコピーする。コピーした件数を返す。
pub fn copy_settings(settings: &[(String, String)], bot: &Db) -> usize {
    let c = lock(&bot.c);
    let _ = c.execute_batch("CREATE TABLE IF NOT EXISTS bot_settings(k TEXT PRIMARY KEY, v TEXT)");
    settings.iter().filter(|(k, v)| c.execute("INSERT OR IGNORE INTO bot_settings(k,v) VALUES(?,?)", [k, v]).map_or(false, |n| n > 0)).count()
}

/// 登録中のフレンドのチャンネルが、実在してBotから見えるかを確認する。
pub fn check_channels(bot: &Db, discord: &dyn Discord) -> Vec<(Friend, Result<bool, DiscordError>)> {
    bot.active()
        .unwrap_or_default()
        .into_iter()
        .map(|f| {
            let r = match f.channel_id {
                Some(ch) => discord.channel_exists(ch),
                None => Ok(false),
            };
            (f, r)
        })
        .collect()
}
