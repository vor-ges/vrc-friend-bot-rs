//! テスト用の簡易HTTPサーバー。VRChat APIの代わりに、決めた応答を返して届いたリクエストを記録する。
#![allow(dead_code)]
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Req {
    pub method: String,
    pub path: String, // クエリを含む
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Req {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

pub struct Resp {
    pub status: u16,
    pub body: String,
    pub set_cookies: Vec<String>,
    pub delay_ms: u64,
}

impl Resp {
    pub fn json(body: &str) -> Self {
        Resp { status: 200, body: body.into(), set_cookies: vec![], delay_ms: 0 }
    }
    pub fn status(code: u16, body: &str) -> Self {
        Resp { status: code, body: body.into(), set_cookies: vec![], delay_ms: 0 }
    }
    pub fn cookie(mut self, c: &str) -> Self {
        self.set_cookies.push(c.into());
        self
    }
    pub fn delay(mut self, ms: u64) -> Self {
        self.delay_ms = ms;
        self
    }
}

pub struct Mock {
    pub base: String,
    pub reqs: Arc<Mutex<Vec<Req>>>,
}

impl Mock {
    pub fn requests(&self) -> Vec<Req> {
        self.reqs.lock().unwrap().clone()
    }
    pub fn paths(&self) -> Vec<String> {
        self.requests().iter().map(|r| format!("{} {}", r.method, r.path)).collect()
    }
}

fn read_req(s: &mut TcpStream) -> Option<Req> {
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buf = vec![];
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
    let headers: Vec<(String, String)> =
        lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
    let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Some(Req { method, path, headers, body: String::from_utf8_lossy(&body).to_string() })
}

pub fn spawn(handler: impl Fn(&Req) -> Resp + Send + Sync + 'static) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let reqs = Arc::new(Mutex::new(vec![]));
    let (log, handler) = (reqs.clone(), Arc::new(handler));
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            let (log, handler) = (log.clone(), handler.clone());
            std::thread::spawn(move || {
                let Some(req) = read_req(&mut s) else { return };
                log.lock().unwrap().push(req.clone());
                let r = handler(&req);
                std::thread::sleep(Duration::from_millis(r.delay_ms));
                let mut out = format!("HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", r.status, r.body.len());
                for c in &r.set_cookies {
                    out.push_str(&format!("Set-Cookie: {c}\r\n"));
                }
                out.push_str("\r\n");
                out.push_str(&r.body);
                let _ = s.write_all(out.as_bytes());
            });
        }
    });
    Mock { base, reqs }
}

use chrono::{Duration as CDuration, TimeZone};
use chrono_tz::Asia::Tokyo;

/// T(d,h,m) = 10日前の0:00(JST) + (d-1)日 + h時間 + m分 のUNIX秒 (test_local.py の T と同じ)
pub fn t(d: i64, h: i64, m: i64) -> f64 {
    let today = Tokyo.timestamp_opt(vrc_friend_bot_rs::db::now() as i64, 0).unwrap().date_naive();
    let base = Tokyo.from_local_datetime(&today.and_hms_opt(0, 0, 0).unwrap()).unwrap() - CDuration::days(10);
    (base + CDuration::days(d - 1) + CDuration::hours(h) + CDuration::minutes(m)).timestamp() as f64
}
