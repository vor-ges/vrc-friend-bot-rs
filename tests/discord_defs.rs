//! test_local.py の「コマンド体系」「スラッシュコマンド定義」の移植 (Discordに送る定義そのものを検証する)。
#![cfg(feature = "bot")]
use serde_json::{json, Value};
use vrc_friend_bot_rs::discord::command_defs;

fn defs() -> Vec<Value> {
    command_defs().iter().map(|c| serde_json::to_value(c).unwrap()).collect()
}
fn by<'a>(d: &'a [Value], name: &str) -> &'a Value {
    d.iter().find(|x| x["name"] == name).unwrap()
}
fn opts(c: &Value) -> Vec<Value> {
    c["options"].as_array().cloned().unwrap_or_default()
}

#[test]
fn commands_are_flat_one_word_commands() {
    let d = defs();
    let mut names: Vec<&str> = d.iter().map(|x| x["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["add", "lang", "list", "mention", "remove", "stats"]);
    for c in &d {
        let n = c["name"].as_str().unwrap();
        assert!(n.chars().all(|ch| ch.is_ascii_lowercase()), "{n}"); // 1語・グループではない
        assert!(c["description"].as_str().unwrap().chars().count() <= 100, "{n}");
        for o in opts(c) {
            assert!(![1, 2].contains(&o["type"].as_i64().unwrap()), "サブコマンド/グループは使わない: {n}"); // フラットなまま
            assert!(o["description"].as_str().unwrap().chars().count() <= 100, "{n}.{}", o["name"]);
        }
    }
}

#[test]
fn option_definitions() {
    let d = defs();
    for c in ["add", "remove"] {
        let o = opts(by(&d, c));
        assert_eq!(o.len(), 1);
        assert!(o[0]["name"] == "target" && o[0]["type"] == 3 && o[0]["required"] == true && o[0]["autocomplete"] == true, "{c}: {o:?}");
    }
    let l = opts(by(&d, "list"));
    assert_eq!(l.iter().map(|o| (o["name"].as_str().unwrap(), o["type"].as_i64().unwrap())).collect::<Vec<_>>(), [("show_uid", 5), ("include_removed", 5)]);
    assert!(l.iter().all(|o| o["required"] != json!(true)));

    let s = opts(by(&d, "stats"));
    assert_eq!(s.iter().map(|o| o["name"].as_str().unwrap()).collect::<Vec<_>>(), ["days", "who"]);
    assert!(s[0]["type"] == 4 && s[0]["required"] != json!(true) && s[0]["min_value"] == 1 && s[0]["max_value"] == 365, "{:?}", s[0]);
    assert!(s[1]["type"] == 3 && s[1]["required"] != json!(true) && s[1]["autocomplete"] == true);

    let g = opts(by(&d, "lang"));
    assert!(g[0]["name"] == "code" && g[0]["type"] == 3 && g[0]["required"] == true && g[0]["min_length"] == 2 && g[0]["max_length"] == 16, "{:?}", g[0]);

    let m = opts(by(&d, "mention"));
    assert!(m[0]["name"] == "state" && m[0]["required"] == true);
    let choices: Vec<&str> = m[0]["choices"].as_array().unwrap().iter().map(|c| c["value"].as_str().unwrap()).collect();
    assert_eq!(choices, ["on", "off", "reset"]);
}
