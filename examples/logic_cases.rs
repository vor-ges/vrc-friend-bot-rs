//! tools/diff_logic.py が作った入力(/tmp/logic_in.json)を logic.rs に通して、結果(/tmp/logic_rs.json)を出す。
use chrono_tz::Tz;
use serde_json::{json, Value};
use std::collections::HashMap;
use vrc_friend_bot_rs::db::Friend;
use vrc_friend_bot_rs::i18n;
use vrc_friend_bot_rs::logic::*;
use vrc_friend_bot_rs::stats::{Stat, Summary};

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}
fn opt(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

fn stat_from(v: &Value) -> Stat {
    // StatusMap は add() でしか作れないので、順序を保ったまま積む
    let mut st = Stat { logins: v["logins"].as_u64().unwrap() as u32, seconds: v["seconds"].as_f64().unwrap(), ..Default::default() };
    for (i, h) in v["hours"].as_array().unwrap().iter().enumerate() {
        st.hours[i] = h.as_u64().unwrap() as u32;
    }
    for kv in v["status"].as_array().unwrap() {
        st.status.add(kv[0].as_str().unwrap(), kv[1].as_f64().unwrap());
    }
    st
}

fn main() {
    let input: Value = serde_json::from_str(&std::fs::read_to_string("/tmp/logic_in.json").unwrap()).unwrap();
    let mut out = serde_json::Map::new();

    out.insert("esc".into(), input["esc"].as_array().unwrap().iter().map(|x| json!(esc(x.as_str().unwrap()))).collect());
    out.insert("slug".into(), input["slug"].as_array().unwrap().iter().map(|x| json!(slug(x[0].as_str().unwrap(), x[1].as_str().unwrap()))).collect());
    out.insert(
        "loc".into(),
        input["loc"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| match parse_location(x.as_str().unwrap()) {
                None => Value::Null,
                Some(l) => json!([l.wid, l.iid, l.label, l.region, l.owner.map(|(k, id)| json!([if k == OwnerKind::Grp { "grp" } else { "usr" }, id]))]),
            })
            .collect(),
    );
    out.insert(
        "choices".into(),
        input["choices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let rows: Vec<Person> = c["rows"].as_array().unwrap().iter().map(|r| Person { uid: s(&r["uid"]), name: opt(&r["name"]) }).collect();
                make_choices(&rows, c["current"].as_str().unwrap()).iter().map(|ch| json!([ch.name, ch.value])).collect::<Value>()
            })
            .collect(),
    );

    let mut stats_out = vec![];
    for c in input["stats"].as_array().unwrap() {
        i18n::set_lang(c["lang"].as_str().unwrap()).unwrap();
        let tz: Tz = "Asia/Tokyo".parse().unwrap();
        let per: HashMap<String, Stat> = c["per"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), stat_from(v))).collect();
        let res = Summary { per, total: stat_from(&c["total"]) };
        let names: Option<Vec<(String, Option<String>)>> =
            c["names"].as_array().map(|a| a.iter().map(|p| (s(&p[0]), opt(&p[1]))).collect());
        let e = stats_embed(c["title"].as_str().unwrap(), &res, c["start"].as_f64().unwrap(), c["end"].as_f64().unwrap(), tz, names.as_deref());
        stats_out.push(e.to_value());
    }
    out.insert("stats".into(), Value::Array(stats_out));

    let mut list_out = vec![];
    for c in input["lists"].as_array().unwrap() {
        i18n::set_lang(c["lang"].as_str().unwrap()).unwrap();
        let rows: Vec<Friend> = c["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| Friend { uid: s(&r["uid"]), name: opt(&r["name"]), channel_id: r["channel_id"].as_i64(), active: r["active"].as_i64().unwrap() != 0, added_at: None })
            .collect();
        let mention = c["mention"].as_bool();
        let overrides: HashMap<String, bool> = c["overrides"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_bool().unwrap())).collect();
        let mut lv = ListView::new(rows, c["show_uid"].as_bool().unwrap(), mention, overrides);
        let mut pages = vec![];
        for p in 0..lv.pages {
            lv.page = p;
            pages.push(json!({"embed": lv.embed().to_value(), "prev_disabled": lv.prev_disabled(), "next_disabled": lv.next_disabled()}));
        }
        list_out.push(json!({"pages": lv.pages, "views": pages}));
    }
    out.insert("lists".into(), Value::Array(list_out));
    std::fs::write("/tmp/logic_rs.json", Value::Object(out).to_string()).unwrap();
}
