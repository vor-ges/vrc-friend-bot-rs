//! i18n.py の移植: 表示言語の切り替え。
//!
//! 日本語の文をそのままキーにして `tr!` で包む方式(gettext風)。
//! - ja: 表を引かずそのまま出す(従来の表示と完全に同じ)
//! - 他言語: `i18n_table.rs` の表から引く。表に無い文は日本語のまま出す
//! - 訳は Python版 i18n.py から `tools/gen_i18n_table.py` で生成する(手で編集しない)
//! - 選択中の言語は DB(bot_settings)に保存され、再起動後も保たれる
use crate::db::{lock, Conn};
use crate::i18n_table as t;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

pub use t::table;

static LANG: RwLock<&'static str> = RwLock::new("ja");

pub fn langs() -> &'static [(&'static str, &'static str)] {
    t::LANGS
}

/// ユーザー入力(en / ja / jpn / jp / en-US など)を LANGS のコードに変換。不明なら None。
pub fn resolve(text: &str) -> Option<&'static str> {
    let s = text.trim().to_lowercase().replace('_', "-");
    let head = s.split('-').next().unwrap_or("");
    let code = t::CODES.iter().find(|(k, _)| *k == head)?.1;
    t::LANGS.iter().find(|(k, _)| *k == code).map(|(k, _)| *k)
}

pub fn get_lang() -> &'static str {
    *LANG.read().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, PartialEq, Eq)]
pub struct UnsupportedLang(pub String);

impl std::fmt::Display for UnsupportedLang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unsupported language: {}", self.0)
    }
}
impl std::error::Error for UnsupportedLang {}

pub fn set_lang(code: &str) -> Result<(), UnsupportedLang> {
    let k = t::LANGS.iter().find(|(k, _)| *k == code).map(|(k, _)| *k).ok_or_else(|| UnsupportedLang(code.to_string()))?;
    *LANG.write().unwrap_or_else(|e| e.into_inner()) = k;
    Ok(())
}

// ---------- 書式 (Python の str.format の必要な部分) ----------

/// `tr!` に渡せる引数。
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Str(String),
    Int(i64),
    Float(f64),
}

macro_rules! arg_from {
    ($variant:ident, $conv:expr; $($t:ty),+) => {
        $(impl From<$t> for Arg { fn from(v: $t) -> Self { Arg::$variant($conv(v)) } })+
    };
}
arg_from!(Int, |v| v as i64; i8, i16, i32, i64, u8, u16, u32, u64, usize, isize);
arg_from!(Float, |v| v as f64; f32, f64);
impl From<&str> for Arg {
    fn from(v: &str) -> Self {
        Arg::Str(v.to_string())
    }
}
impl From<String> for Arg {
    fn from(v: String) -> Self {
        Arg::Str(v)
    }
}
impl From<&String> for Arg {
    fn from(v: &String) -> Self {
        Arg::Str(v.clone())
    }
}

struct Spec {
    zero: bool,
    width: usize,
    prec: Option<usize>,
    ty: Option<char>,
}

fn parse_spec(s: &str) -> Option<Spec> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let zero = b.first() == Some(&'0');
    if zero {
        i += 1;
    }
    let mut width = 0usize;
    while let Some(d) = b.get(i).and_then(|c| c.to_digit(10)) {
        width = width.checked_mul(10)?.checked_add(d as usize)?;
        i += 1;
    }
    let mut prec = None;
    if b.get(i) == Some(&'.') {
        i += 1;
        let mut p = 0usize;
        let start = i;
        while let Some(d) = b.get(i).and_then(|c| c.to_digit(10)) {
            p = p.checked_mul(10)?.checked_add(d as usize)?;
            i += 1;
        }
        if i == start {
            return None;
        }
        prec = Some(p);
    }
    let ty = b.get(i).copied();
    if ty.is_some() {
        i += 1;
    }
    if i != b.len() {
        return None;
    }
    Some(Spec { zero, width, prec, ty })
}

fn pad(s: String, width: usize, zero: bool, numeric: bool) -> String {
    let n = s.chars().count();
    if n >= width {
        return s;
    }
    let fill = width - n;
    if numeric && zero {
        let (sign, digits) = match s.strip_prefix('-') {
            Some(rest) => ("-", rest),
            None => ("", s.as_str()),
        };
        format!("{sign}{}{digits}", "0".repeat(fill))
    } else if numeric {
        format!("{}{s}", " ".repeat(fill))
    } else {
        format!("{s}{}", " ".repeat(fill))
    }
}

fn render(arg: &Arg, spec: &str) -> Option<String> {
    if spec.is_empty() {
        return Some(match arg {
            Arg::Str(s) => s.clone(),
            Arg::Int(i) => i.to_string(),
            Arg::Float(f) => format!("{f:?}"),
        });
    }
    let sp = parse_spec(spec)?;
    match arg {
        Arg::Str(s) => {
            if sp.zero || !matches!(sp.ty, None | Some('s')) {
                return None;
            }
            let s: String = match sp.prec {
                Some(p) => s.chars().take(p).collect(),
                None => s.clone(),
            };
            Some(pad(s, sp.width, false, false))
        }
        Arg::Int(i) => match sp.ty {
            None | Some('d') if sp.prec.is_none() => Some(pad(i.to_string(), sp.width, sp.zero, true)),
            Some('f') => Some(pad(format!("{:.*}", sp.prec.unwrap_or(6), *i as f64), sp.width, sp.zero, true)),
            _ => None,
        },
        Arg::Float(f) => match sp.ty {
            Some('f') => Some(pad(format!("{:.*}", sp.prec.unwrap_or(6), f), sp.width, sp.zero, true)),
            _ => None,
        },
    }
}

/// Python の `template.format(**args)` 相当。KeyError / ValueError に当たるものは Err。
pub fn format(template: &str, args: &[(&str, Arg)]) -> Result<String, ()> {
    let mut out = String::with_capacity(template.len() + 16);
    let mut it = template.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '{' => {
                if it.peek() == Some(&'{') {
                    it.next();
                    out.push('{');
                    continue;
                }
                let mut field = String::new();
                loop {
                    match it.next() {
                        Some('}') => break,
                        Some('{') | None => return Err(()),
                        Some(ch) => field.push(ch),
                    }
                }
                let (name, spec) = field.split_once(':').unwrap_or((field.as_str(), ""));
                if name.is_empty() || name.contains(['.', '[', '!']) {
                    return Err(());
                }
                let arg = args.iter().find(|(k, _)| *k == name).map(|(_, v)| v).ok_or(())?;
                out.push_str(&render(arg, spec).ok_or(())?);
            }
            '}' => {
                if it.peek() == Some(&'}') {
                    it.next();
                    out.push('}');
                } else {
                    return Err(());
                }
            }
            _ => out.push(c),
        }
    }
    Ok(out)
}

fn en_map(lang: &str) -> Option<&'static HashMap<&'static str, &'static str>> {
    static MAPS: OnceLock<HashMap<&'static str, HashMap<&'static str, &'static str>>> = OnceLock::new();
    let maps = MAPS.get_or_init(|| {
        t::LANGS
            .iter()
            .filter_map(|(k, _)| t::table(k).map(|tb| (*k, tb.iter().copied().collect())))
            .collect()
    });
    maps.get(lang)
}

/// 日本語の文を現在の言語に翻訳して format する。表に無ければ日本語のまま。
/// 書式エラーでも例外(panic)にせず、翻訳 → 原文 → 未整形の原文 の順に諦める(通知を止めない)。
pub fn tr(msg: &str, args: &[(&str, Arg)]) -> String {
    let lang = get_lang();
    let s = en_map(lang).and_then(|m| m.get(msg).copied()).unwrap_or(msg);
    if args.is_empty() {
        return s.to_string();
    }
    match format(s, args) {
        Ok(v) => v,
        Err(()) => {
            log::error!("翻訳の書式エラー lang={lang} msg={msg:?}");
            format(msg, args).unwrap_or_else(|()| msg.to_string())
        }
    }
}

/// `tr!("{name} が移動", name = n)` のように使う。
#[macro_export]
macro_rules! tr {
    ($msg:expr $(,)?) => {
        $crate::i18n::tr($msg, &[])
    };
    ($msg:expr, $($k:ident = $v:expr),+ $(,)?) => {
        $crate::i18n::tr($msg, &[$((stringify!($k), $crate::i18n::Arg::from($v))),+])
    };
}

// ---------- 永続化 ----------
fn ensure(c: &rusqlite::Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS bot_settings(k TEXT PRIMARY KEY, v TEXT)")
}

/// DBから言語設定を読み込んで反映する(無い/壊れていれば ja)。
pub fn load(conn: &Conn) -> rusqlite::Result<&'static str> {
    let c = lock(conn);
    ensure(&c)?;
    let v: Option<String> = c
        .query_row("SELECT v FROM bot_settings WHERE k='lang'", [], |r| r.get(0))
        .map(Some)
        .or_else(|e| if matches!(e, rusqlite::Error::QueryReturnedNoRows) { Ok(None) } else { Err(e) })?;
    let code = v.as_deref().filter(|c| t::LANGS.iter().any(|(k, _)| k == c)).unwrap_or("ja");
    let _ = set_lang(code);
    Ok(get_lang())
}

pub fn save(conn: &Conn, code: &str) -> rusqlite::Result<()> {
    let c = lock(conn);
    ensure(&c)?;
    c.execute(
        "INSERT INTO bot_settings(k,v) VALUES('lang',?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        [code],
    )?;
    Ok(())
}
