//! tools/parity_bench.py が作った /tmp/bench.db と /tmp/queries.tsv を集計して、結果と所要時間をJSONで出す。
use std::fmt::Write as _;
use std::time::Instant;
use vrc_friend_bot_rs::db::Conn;
use vrc_friend_bot_rs::stats::{make_tz, Stat, Stats};

fn stat_json(s: &Stat) -> String {
    let mut o = String::new();
    write!(o, "{{\"logins\":{},\"hours\":{:?},\"status\":[", s.logins, s.hours).unwrap();
    for (i, (k, v)) in s.status.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        write!(o, "[\"{k}\",{v:?}]").unwrap();
    }
    write!(o, "],\"seconds\":{:?}}}", s.seconds).unwrap();
    o
}

fn rss_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = |key: &str| s.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    kb("VmHWM") / 1024.0
}

fn main() {
    let conn: Conn = std::sync::Arc::new(std::sync::Mutex::new(rusqlite::Connection::open("/tmp/bench.db").unwrap()));
    let stats = Stats::new(conn, make_tz("Asia/Tokyo")).unwrap();
    let mut out = String::from("{");
    for (qi, line) in std::fs::read_to_string("/tmp/queries.tsv").unwrap().lines().enumerate() {
        let p: Vec<&str> = line.split('\t').collect();
        let (name, start, end) = (p[0], p[1].parse::<f64>().unwrap(), p[2].parse::<f64>().unwrap());
        let uids: Vec<&str> = p[3].split(',').collect();
        let mut best = f64::MAX;
        let mut res = None;
        for _ in 0..5 {
            let t0 = Instant::now();
            res = Some(stats.summarize(&uids, start, end).unwrap());
            best = best.min(t0.elapsed().as_secs_f64() * 1000.0);
        }
        let res = res.unwrap();
        if qi > 0 {
            out.push(',');
        }
        write!(out, "\"{name}\":{{\"ms\":{best:?},\"total\":{},\"per\":{{", stat_json(&res.total)).unwrap();
        for (i, u) in uids.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            write!(out, "\"{u}\":{}", stat_json(&res.per[*u])).unwrap();
        }
        out.push_str("}}");
        eprintln!("rust   {name:8} {best:8.1} ms");
    }
    out.push('}');
    std::fs::write("/tmp/rs_out.json", out).unwrap();
    eprintln!("peak RSS: {:.1} MB", rss_mb());
}
