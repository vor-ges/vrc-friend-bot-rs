#!/usr/bin/env python3
"""Python版 stats.py と Rust版 summarize の出力一致 + 速度比較。
  python3 tools/parity_bench.py gen   -> /tmp/bench.db と /tmp/queries.tsv を作り、Pythonの結果を /tmp/py_out.json に保存
  (cargo run --release --example bench で Rust の結果を /tmp/rs_out.json に保存)
  python3 tools/parity_bench.py cmp   -> 2つの結果を突き合わせる
"""
import sys, json, random, sqlite3, time, os, pathlib
root = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(root / "upstream"))
from stats import Stats, make_tz

def stat_json(p):
    return {"logins": p["logins"], "hours": p["hours"], "status": list(p["status"].items()), "seconds": p["seconds"]}

if sys.argv[1] == "gen":
    for f in ("/tmp/bench.db",):
        if os.path.exists(f): os.remove(f)
    c = sqlite3.connect("/tmp/bench.db"); c.row_factory = sqlite3.Row
    s = Stats(c, make_tz("Asia/Tokyo"))
    now = time.time(); random.seed(1)
    uids = [f"usr_{i:08x}" for i in range(300)]
    statuses = ["active", "join me", "ask me", "busy"]
    rows = []
    for u in uids:
        t = now - 365 * 86400
        while t < now:
            t += random.uniform(3600, 40 * 3600)
            rows.append((u, t, "login", random.choice(statuses)))
            for _ in range(random.randint(0, 2)):
                rows.append((u, t + random.uniform(60, 3000), "status", random.choice(statuses)))
            rows.append((u, t + random.uniform(3600, 14400), "logout", None))
            if random.random() < 0.05:  # 二重ログイン等の乱れも混ぜる
                rows.append((u, t + 10, "login", "busy"))
    c.executemany("INSERT INTO activity(uid,ts,kind,status) VALUES(?,?,?,?)", rows); c.commit()
    queries = [("all_1y", uids, now - 365 * 86400, now), ("all_30d", uids, now - 30 * 86400, now),
               ("one_1y", uids[:1], now - 365 * 86400, now), ("ten_90d", uids[:10], now - 90 * 86400, now)]
    with open("/tmp/queries.tsv", "w") as f:
        for n, us, a, b in queries: f.write(f"{n}\t{a!r}\t{b!r}\t{','.join(us)}\n")
    out = {}
    print("rows:", len(rows))
    for n, us, a, b in queries:
        best = 1e9
        for _ in range(5):
            t0 = time.perf_counter(); res = s.summarize(us, a, b); best = min(best, time.perf_counter() - t0)
        out[n] = {"ms": best * 1000, "total": stat_json(res["total"]), "per": {u: stat_json(res["per"][u]) for u in us}}
        print(f"python {n:8s} {best*1000:8.1f} ms")
    json.dump(out, open("/tmp/py_out.json", "w"))
else:
    py, rs = json.load(open("/tmp/py_out.json")), json.load(open("/tmp/rs_out.json"))
    ok = True
    for n in py:
        a, b = py[n], rs[n]
        same_total = a["total"] == b["total"]
        diffs = [u for u in a["per"] if a["per"][u] != b["per"].get(u)]
        # 浮動小数の最終桁だけの違いも検出するため厳密比較 + 近似比較の両方を出す
        close = all(abs(a["per"][u]["seconds"] - b["per"][u]["seconds"]) < 1e-6 for u in a["per"])
        print(f"{n:8s} total一致={same_total} 人別の不一致={len(diffs)}/{len(a['per'])} 秒の誤差<1e-6={close}   python {a['ms']:.1f} ms  rust {b['ms']:.1f} ms  ({a['ms']/b['ms']:.1f}x)")
        ok &= same_total and not diffs
    print("PARITY OK" if ok else "PARITY MISMATCH")
