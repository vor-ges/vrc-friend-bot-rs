//! bot.py の純粋なロジック部分の移植 (Discord / VRChat / DB に触れない): 場所の解析、名前の整形、
//! オートコンプリート候補、統計・一覧の埋め込み (Embed)、一覧のページ送り。
use crate::db::Friend;
use crate::stats::Summary;
use crate::tr;
use chrono::TimeZone;
use chrono_tz::Tz;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use unicode_general_category::{get_general_category as gc, GeneralCategory as G};

/// ステータス名 -> (絵文字, 色)
pub const STATUS: &[(&str, &str, u32)] = &[
    ("join me", "🔵", 0x3498DB),
    ("active", "🟢", 0x2ECC71),
    ("ask me", "🟠", 0xE67E22),
    ("busy", "🔴", 0xE74C3C),
    ("offline", "⚫", 0x95A5A6),
];

pub fn status_style(st: &str) -> Option<(&'static str, u32)> {
    STATUS.iter().find(|(k, _, _)| *k == st).map(|(_, e, c)| (*e, *c))
}

// ---------- 文字列の小道具 (Pythonのスライスは文字(コードポイント)単位) ----------

pub fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub fn last_chars(s: &str, n: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    c[c.len().saturating_sub(n)..].iter().collect()
}

/// Pythonの round() (偶数丸め: round(2.5) == 2)。
pub fn py_round(x: f64) -> i64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 {
        (2.0 * (x / 2.0).round()) as i64
    } else {
        r as i64
    }
}

// ---------- 場所 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerKind {
    Grp,
    Usr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Loc {
    pub wid: String,
    pub iid: String,
    pub label: &'static str,
    pub region: String,
    pub owner: Option<(OwnerKind, String)>,
}

/// location文字列 -> 解析結果。private / offline / traveling など特殊値は None。
pub fn parse_location(loc: &str) -> Option<Loc> {
    if loc.is_empty() || matches!(loc, "offline" | "private" | "traveling") || !loc.contains(':') {
        return None;
    }
    let (wid, rest) = loc.split_once(':')?;
    let mut parts = rest.split('~');
    let _iid = parts.next();
    let mut t: HashMap<&str, &str> = HashMap::new();
    for x in parts {
        let key = x.split('(').next().unwrap_or("");
        let val = match x.split_once('(') {
            Some((_, v)) => v.trim_end_matches(')'),
            None => "",
        };
        t.insert(key, val);
    }
    let region = t.get("region").copied().unwrap_or("us").to_string();
    let label = if t.contains_key("group") {
        match t.get("groupAccessType").copied() {
            Some("public") => "Group Public",
            Some("plus") => "Group+",
            _ => "Group",
        }
    } else if t.contains_key("hidden") {
        "Friends+"
    } else if t.contains_key("friends") {
        "Friends"
    } else if t.contains_key("private") {
        if t.contains_key("canRequestInvite") {
            "Invite+"
        } else {
            "Invite"
        }
    } else {
        "Public"
    };
    let owner = match t.get("group").filter(|v| !v.is_empty()) {
        Some(g) => Some((OwnerKind::Grp, g.to_string())),
        None => ["hidden", "friends", "private"]
            .iter()
            .find_map(|k| t.get(k).filter(|v| !v.is_empty()).map(|v| (OwnerKind::Usr, v.to_string()))),
    };
    Some(Loc { wid: wid.to_string(), iid: rest.to_string(), label, region, owner })
}

// ---------- 名前 ----------

/// Pythonの正規表現 \w : 文字・数字・アンダースコア (str.isalnum() または '_')
fn is_word(c: char) -> bool {
    c == '_' || matches!(gc(c), G::UppercaseLetter | G::LowercaseLetter | G::TitlecaseLetter | G::ModifierLetter | G::OtherLetter | G::DecimalNumber | G::LetterNumber | G::OtherNumber)
}

/// チャンネル名: 小文字化し、英数字・_・- 以外の連続を - にまとめる。空なら UID の末尾8文字。最大90文字。
pub fn slug(name: &str, uid: &str) -> String {
    let lower = name.to_lowercase();
    let mut s = String::new();
    let mut in_bad = false;
    for c in lower.chars() {
        if is_word(c) || c == '-' {
            s.push(c);
            in_bad = false;
        } else if !in_bad {
            s.push('-');
            in_bad = true;
        }
    }
    let s = s.trim_matches('-');
    let s = if s.is_empty() { last_chars(uid, 8) } else { s.to_string() };
    take_chars(&s, 90)
}

/// Pythonの \s (str.isspace): Unicodeの空白 + 0x1c-0x1f
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn escape_mentions(s: &str) -> String {
    // re.sub(r'@(everyone|here|[!&]?[0-9]{17,20})', '@\u200b\\1', text)
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        out.push(c[i]);
        if c[i] == '@' {
            let rest: String = c[i + 1..].iter().take(8).collect();
            let hit = if rest.starts_with("everyone") || rest.starts_with("here") {
                true
            } else {
                let mut j = i + 1;
                if matches!(c.get(j), Some('!') | Some('&')) {
                    j += 1;
                }
                let digits = c[j.min(c.len())..].iter().take_while(|d| d.is_ascii_digit()).count();
                digits >= 17
            };
            if hit {
                out.push('\u{200b}');
            }
        }
        i += 1;
    }
    out
}

fn url_len(c: &[char], i: usize) -> Option<usize> {
    // <[^: >]+:\/[^ >]+>
    if c[i] == '<' {
        let mut j = i + 1;
        while j < c.len() && !matches!(c[j], ':' | ' ' | '>') {
            j += 1;
        }
        if j > i + 1 && c.get(j) == Some(&':') && c.get(j + 1) == Some(&'/') {
            let mut k = j + 2;
            while k < c.len() && !matches!(c[k], ' ' | '>') {
                k += 1;
            }
            if k > j + 2 && c.get(k) == Some(&'>') {
                return Some(k + 1 - i);
            }
        }
    }
    // (?:https?|steam):\/\/[^\s<]+[^<.,:;"'\]\s]
    for scheme in ["https", "http", "steam"] {
        let sc: Vec<char> = scheme.chars().collect();
        let k = i + sc.len();
        if c.len() >= k + 3 && c[i..k] == sc[..] && c[k] == ':' && c[k + 1] == '/' && c[k + 2] == '/' {
            let start = k + 3;
            let mut end = start;
            while end < c.len() && !(is_py_space(c[end]) || c[end] == '<') {
                end += 1;
            }
            let mut p = end;
            while p > start + 1 {
                p -= 1;
                if !matches!(c[p], '.' | ',' | ':' | ';' | '"' | '\'' | ']') {
                    return Some(p + 1 - i);
                }
            }
        }
    }
    None
}

fn stock_len(c: &[char], i: usize) -> Option<usize> {
    let line_start = i == 0 || c[i - 1] == '\n';
    if matches!(c[i], '_' | '\\' | '~' | '|' | '*' | '`') {
        return Some(1);
    }
    // ^>(?:>>)?\s
    if line_start && c[i] == '>' {
        if c.get(i + 1) == Some(&'>') && c.get(i + 2) == Some(&'>') && c.get(i + 3).map_or(false, |&x| is_py_space(x)) {
            return Some(4);
        }
        if c.get(i + 1).map_or(false, |&x| is_py_space(x)) {
            return Some(2);
        }
    }
    // \[.+\]\(.+\)  (行内で最長)
    if c[i] == '[' {
        let line_end = c[i..].iter().position(|&x| x == '\n').map_or(c.len(), |p| i + p);
        let last_paren = (i..line_end).rev().find(|&k| c[k] == ')');
        if let Some(y) = last_paren {
            let mut x = line_end.saturating_sub(2);
            while x >= i + 2 {
                if c[x] == ']' && c.get(x + 1) == Some(&'(') && y >= x + 3 {
                    return Some(y + 1 - i);
                }
                if x == 0 {
                    break;
                }
                x -= 1;
            }
        }
    }
    // ^#{1,3}
    if line_start && c[i] == '#' {
        return Some(c[i..].iter().take(3).take_while(|&&x| x == '#').count());
    }
    // ^\s*-
    if line_start {
        let mut j = i;
        while j < c.len() && is_py_space(c[j]) {
            j += 1;
        }
        if c.get(j) == Some(&'-') {
            return Some(j + 1 - i);
        }
    }
    None
}

fn escape_markdown(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 8);
    let mut i = 0;
    while i < c.len() {
        if let Some(n) = url_len(&c, i) {
            out.extend(&c[i..i + n]); // URLはそのまま
            i += n;
        } else if let Some(n) = stock_len(&c, i) {
            out.push('\\');
            out.extend(&c[i..i + n]);
            i += n;
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

/// Discordのメンションとマークダウンを無効化する (discord.utils.escape_markdown(escape_mentions(s)))。
pub fn esc(s: &str) -> String {
    escape_markdown(&escape_mentions(s))
}

// ---------- オートコンプリート ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub uid: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub name: String,
    pub value: String,
}

/// オートコンプリート候補(Discordの上限25件)。値はUID。
/// 同名・類似名でも区別できるようUID下6桁を付ける(衝突時は12桁→連番)。
pub fn make_choices(rows: &[Person], current: &str) -> Vec<Choice> {
    let cur = current.trim().to_lowercase();
    let mut out = vec![];
    let mut seen: HashSet<String> = HashSet::new();
    for r in rows {
        let name = r.name.as_deref().filter(|n| !n.is_empty()).unwrap_or(&r.uid);
        if !cur.is_empty() && !name.to_lowercase().contains(&cur) && !r.uid.contains(&cur) {
            continue;
        }
        let mut label = format!("{} (…{})", take_chars(name, 80), last_chars(&r.uid, 6));
        if seen.contains(&label) {
            label = format!("{} (…{})", take_chars(name, 70), last_chars(&r.uid, 12));
        }
        let mut n = 1;
        while seen.contains(&label) {
            n += 1;
            label = format!("{} (…{}) #{n}", take_chars(name, 70), last_chars(&r.uid, 12));
        }
        seen.insert(label.clone());
        out.push(Choice { name: take_chars(&label, 100), value: r.uid.clone() });
        if out.len() >= 25 {
            break;
        }
    }
    out
}

// ---------- Embed ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

/// Discordに依存しない埋め込み。discord.rs が実際のDiscordの形に変換する。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Embed {
    pub title: Option<String>,
    pub description: Option<String>,
    pub color: Option<u32>,
    pub fields: Vec<Field>,
    pub thumbnail: Option<String>,
    pub footer: Option<String>,
}

impl Embed {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn title(mut self, t: impl Into<String>) -> Self {
        self.title = Some(t.into());
        self
    }
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }
    pub fn color(mut self, c: u32) -> Self {
        self.color = Some(c);
        self
    }
    pub fn field(mut self, name: impl Into<String>, value: impl Into<String>, inline: bool) -> Self {
        self.add_field(name, value, inline);
        self
    }
    pub fn add_field(&mut self, name: impl Into<String>, value: impl Into<String>, inline: bool) {
        self.fields.push(Field { name: name.into(), value: value.into(), inline });
    }
    pub fn footer(mut self, t: impl Into<String>) -> Self {
        self.footer = Some(t.into());
        self
    }
    /// 埋め込み全体の文字数 (title + description + footer + 各fieldのname/value)。Discordの上限は6000。
    pub fn len(&self) -> usize {
        let c = |s: &Option<String>| s.as_deref().map_or(0, |s| s.chars().count());
        c(&self.title) + c(&self.description) + c(&self.footer) + self.fields.iter().map(|f| f.name.chars().count() + f.value.chars().count()).sum::<usize>()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// discord.py の Embed.to_dict() と同じ形 (type は除く)。テストでの突き合わせ用。
    pub fn to_value(&self) -> Value {
        let mut m = serde_json::Map::new();
        if let Some(t) = &self.title {
            m.insert("title".into(), json!(t));
        }
        if let Some(d) = self.description.as_ref().filter(|d| !d.is_empty()) {
            m.insert("description".into(), json!(d)); // 空の説明はDiscordでも無いのと同じ (discord.pyも出さない)
        }
        if let Some(c) = self.color {
            m.insert("color".into(), json!(c));
        }
        if !self.fields.is_empty() {
            m.insert("fields".into(), self.fields.iter().map(|f| json!({"name": f.name, "value": f.value, "inline": f.inline})).collect());
        }
        if let Some(u) = &self.thumbnail {
            m.insert("thumbnail".into(), json!({"url": u}));
        }
        if let Some(t) = &self.footer {
            m.insert("footer".into(), json!({"text": t}));
        }
        Value::Object(m)
    }
}

// ---------- /stats の埋め込み ----------

fn fmt_ts(ts: f64, tz: Tz) -> String {
    tz.timestamp_opt(ts.floor() as i64, 0).single().map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

/// Stats::summarize の結果を Embed にする。names (uid, 表示名) を渡すとフレンド別も出す。
pub fn stats_embed(title: &str, res: &Summary, start: f64, end: f64, tz: Tz, names: Option<&[(String, Option<String>)]>) -> Embed {
    let tot = &res.total;
    let tzname = tz.timestamp_opt(end.floor() as i64, 0).single().map(|d| d.format("%Z").to_string()).unwrap_or_default();
    let mut e = Embed::new().title(title).color(0x9B59B6);
    e.description = Some(tr!(
        "📅 **集計期間**: {start} 〜 {end} ({tz}) ・約{days:.1f}日間\n🔢 **ゲーム内ログイン**: {logins}回 ・ ゲーム内滞在 {hours:.1f}時間",
        start = fmt_ts(start, tz),
        end = fmt_ts(end, tz),
        tz = tzname,
        days = (end - start) / 86400.0,
        logins = tot.logins,
        hours = tot.seconds / 3600.0
    ));
    let h = &tot.hours;
    let mx = *h.iter().max().unwrap_or(&0);
    if mx == 0 {
        e.add_field(tr!("⏰ ログイン頻度 (時間帯別)"), tr!("まだ記録がありません"), false);
    } else {
        let mut order: Vec<usize> = (0..24).collect();
        order.sort_by_key(|&x| (std::cmp::Reverse(h[x]), x));
        let top = order.iter().take(3).filter(|&&x| h[x] > 0).map(|&x| tr!("{h}時台({c}回)", h = x, c = h[x])).collect::<Vec<_>>().join(" / ");
        let bars: Vec<String> = h
            .iter()
            .enumerate()
            .map(|(x, &c)| {
                let bar = if c > 0 { "█".repeat(std::cmp::max(1, py_round(10.0 * c as f64 / mx as f64)) as usize) } else { String::new() };
                format!("{} {:<10} {}", tr!("{h:02d}時", h = x), bar, c)
            })
            .collect();
        e.add_field(tr!("⏰ ログイン頻度 (時間帯別)"), format!("{}\n```\n{}\n```", tr!("最多: {top}", top = top), bars.join("\n")), false);
    }
    let total = tot.status.total();
    if total <= 0.0 {
        e.add_field(tr!("🎭 ステータス使用割合"), tr!("まだ記録がありません"), false);
    } else {
        let mut ss: Vec<(&str, f64)> = tot.status.iter().collect();
        ss.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)); // 安定ソート: 同率は出現順
        let lines: Vec<String> = ss
            .iter()
            .map(|&(k, v)| {
                tr!(
                    "{emoji} {k} **{pct:.1f}%** ({hours:.1f}時間)",
                    emoji = STATUS.iter().find(|(n, _, _)| *n == k).map_or("❔", |s| s.1),
                    k = if k != "unknown" { k.to_string() } else { tr!("不明") },
                    pct = v / total * 100.0,
                    hours = v / 3600.0
                )
            })
            .collect();
        e.add_field(tr!("🎭 ステータス使用割合 (ゲーム内の滞在時間ベース)"), lines.join("\n"), false);
    }
    if let Some(names) = names.filter(|n| !n.is_empty()) {
        let per = &res.per;
        let lower = |n: &Option<String>| n.clone().unwrap_or_default().to_lowercase();
        let mut order: Vec<&(String, Option<String>)> = names.iter().collect();
        order.sort_by(|a, b| per[&b.0].logins.cmp(&per[&a.0].logins).then_with(|| lower(&a.1).cmp(&lower(&b.1))));
        let mut lines = vec![];
        for (u, name) in order.iter().take(10).map(|x| (&x.0, &x.1)) {
            let (p, nm) = (&per[u], esc(name.as_deref().unwrap_or("")));
            if p.logins > 0 {
                let peak = (0..24).max_by_key(|&x| (p.hours[x], std::cmp::Reverse(x))).unwrap_or(0);
                lines.push(tr!("**{name}** — {logins}回 / 最多 {peak}時台 / {hours:.1f}h", name = nm, logins = p.logins, peak = peak, hours = p.seconds / 3600.0));
            } else if p.seconds > 0.0 {
                lines.push(tr!("**{name}** — {hours:.1f}h (ログイン記録なし)", name = nm, hours = p.seconds / 3600.0));
            } else {
                lines.push(tr!("**{name}** — 記録なし", name = nm));
            }
        }
        if order.len() > 10 {
            lines.push(tr!("…他{n}人", n = order.len() - 10));
        }
        e.add_field(tr!("👥 フレンド別 (ログイン回数順)"), lines.join("\n"), false);
    }
    e.footer = Some(tr!("ゲーム内のログインのみ集計(Webのみは対象外)。フレンドごとの収集開始日時以降の分です。"));
    e
}

// ---------- /list のページ送り ----------

#[derive(Debug, Clone)]
pub struct ListView {
    pub rows: Vec<Friend>,
    pub show_uid: bool,
    pub page: usize,
    pub pages: usize,
    /// None=表示しない / Some(bool)=オンライン通知のメンション(全体設定)
    pub mention: Option<bool>,
    /// フレンド別の設定 {uid: bool}
    pub overrides: HashMap<String, bool>,
}

impl ListView {
    pub const PER: usize = 10;

    pub fn new(rows: Vec<Friend>, show_uid: bool, mention: Option<bool>, overrides: HashMap<String, bool>) -> Self {
        let pages = std::cmp::max(1, rows.len().div_ceil(Self::PER));
        ListView { rows, show_uid, page: 0, pages, mention, overrides }
    }
    pub fn prev_disabled(&self) -> bool {
        self.page == 0
    }
    pub fn next_disabled(&self) -> bool {
        self.page + 1 >= self.pages
    }
    pub fn embed(&self) -> Embed {
        let chunk = &self.rows[(self.page * Self::PER).min(self.rows.len())..((self.page + 1) * Self::PER).min(self.rows.len())];
        let mut lines = vec![];
        for r in chunk {
            let mut custom = String::new();
            let mark = if !r.active {
                tr!("🚫(削除済み)")
            } else if self.mention.is_none() {
                "✅".to_string()
            } else {
                let on = self.overrides.get(&r.uid).copied().unwrap_or(self.mention.unwrap_or(false));
                if self.overrides.contains_key(&r.uid) {
                    custom = format!(" {}", tr!("(個別)"));
                }
                (if on { "🔔" } else { "🔕" }).to_string()
            };
            let mut line = format!("{mark} **{}**{custom} <#{}>", esc(r.name.as_deref().unwrap_or("")), r.channel_id.unwrap_or(0));
            if self.show_uid {
                line += &format!("\n　`{}`", r.uid);
            }
            lines.push(line);
        }
        let active = self.rows.iter().filter(|r| r.active).count();
        let head = match self.mention {
            None => String::new(),
            Some(m) => format!(
                "{}\n{}\n\n",
                if m { tr!("🔔 オンライン通知のメンション(全体設定): オン") } else { tr!("🔕 オンライン通知のメンション(全体設定): オフ") },
                tr!("🔔=メンションあり 🔕=なし (個別)=フレンド別の設定")
            ),
        };
        Embed::new()
            .title(tr!("VRChat 登録フレンド ({n}人)", n = active))
            .description(format!("{head}{}", lines.join("\n")))
            .color(0x3498DB)
            .footer(tr!("ページ {p}/{n}", p = self.page + 1, n = self.pages))
    }
}
