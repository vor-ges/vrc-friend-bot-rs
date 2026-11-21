//! test_local.py の「翻訳の網羅性」の移植。
//! - Rustのソースで使っている `tr!("...")` の全キーに英訳がある
//! - 訳テーブルに、Python版が使っていない古いキーが溜まっていない
//! - Rust側のキーは、Python版と同じ文言だけを使う (勝手に文言を増やさない)
//! bot.rs の移植が済んだら、最後の `ported_all` を true にして「Rust側の全キー = Python側の全キー」を確かめる。
use std::collections::BTreeSet;
use vrc_friend_bot_rs::i18n::table;

/// ソース中の `tr!( "..." ` の文字列リテラルを集める。
fn rust_keys(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = src;
    while let Some(i) = rest.find("tr!(") {
        // `str!(` などの一部にならないこと
        let before = rest[..i].chars().last();
        rest = &rest[i + 4..];
        if before.map_or(false, |c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let r = rest.trim_start();
        let Some(r) = r.strip_prefix('"') else { continue };
        let mut s = String::new();
        let mut it = r.chars();
        while let Some(c) = it.next() {
            match c {
                '"' => break,
                '\\' => match it.next() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('r') => s.push('\r'),
                    Some(o) => s.push(o), // \" \\ \'
                    None => break,
                },
                c => s.push(c),
            }
        }
        out.insert(s);
    }
    out
}

fn ph(s: &str) -> Vec<String> {
    let mut v = vec![];
    let mut cur: Option<String> = None;
    for c in s.chars() {
        match (c, cur.as_mut()) {
            ('{', None) => cur = Some("{".into()),
            ('}', Some(b)) => {
                b.push('}');
                v.push(cur.take().unwrap());
            }
            (_, Some(b)) => b.push(c),
            _ => {}
        }
    }
    v.sort();
    v
}

#[test]
fn tr_keys_are_translated_and_in_sync_with_python() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut used = BTreeSet::new();
    for e in std::fs::read_dir(root.join("src")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().map_or(false, |x| x == "rs") && p.file_name().unwrap() != "i18n_table.rs" {
            // コメント行(使用例を書いた doc コメントなど)は読まない
            let code: String = std::fs::read_to_string(&p).unwrap().lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
            used.extend(rust_keys(&code));
        }
    }
    let py: BTreeSet<String> =
        serde_json::from_str::<Vec<String>>(&std::fs::read_to_string(root.join("tests/data/py_tr_keys.json")).unwrap()).unwrap().into_iter().collect();
    let en = table("en").unwrap();

    assert!(used.len() >= 6, "tr! のキーが拾えていない: {used:?}");
    for k in &used {
        let v = en.iter().find(|(kk, _)| kk == k).unwrap_or_else(|| panic!("英訳が無い: {k:?}")).1;
        assert_eq!(ph(k), ph(v), "プレースホルダが違う: {k:?}");
        assert!(py.contains(k), "Python版に無い文言をRustが使っている: {k:?}");
    }
    let stale: Vec<_> = en.iter().filter(|(k, _)| !py.contains(*k)).map(|(k, _)| *k).collect();
    assert!(stale.is_empty(), "Python版が使っていない訳が溜まっている: {stale:?}");

    let ported_all = true; // bot.rs / discord.rs を移植済み: Python版の全キーをRustも使っている
    if ported_all {
        let missing: Vec<_> = py.difference(&used).collect();
        assert!(missing.is_empty(), "Python版にあってRustに無いキー: {missing:?}");
    }
}
