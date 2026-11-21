#!/usr/bin/env python3
"""bot.py の純粋なロジックを、Python版(本物の bot.py の関数)とRust版(logic.rs)で同じ入力に通して突き合わせる差分テスト。
対象: esc / slug / parse_location / make_choices / stats_embed(ja・en) / ListView(ja・en)
使い方: cargo build --example logic_cases && python3 tools/diff_logic.py"""
import asyncio, json, os, pathlib, random, subprocess, sys, tempfile, time
root = pathlib.Path(__file__).resolve().parent.parent
os.environ.update(DISCORD_TOKEN="x", GUILD_ID="1", MAIN_CHANNEL_ID="2", OWNER_ID="99", VRC_USERNAME="u", VRC_PASSWORD="p")
os.chdir(tempfile.mkdtemp())
sys.path.insert(0, str(root / "upstream"))
import bot as B, i18n, discord
from zoneinfo import ZoneInfo
random.seed(7)

# ---- 入力を作る ----
ALPHA = list("_\\~|*`>#-[]()<>:/hpstmeravnk.,;\"' \n\t@!&0123456789xyz") + ["\u3000", "\u001c", "\u00a0", "\u2028", "あ", "漢", "\u200b", "&", "usr_", "http://", "https://", "steam://", "everyone", "here"]
esc_cases = ["".join(random.choice(ALPHA) for _ in range(random.randint(0, 30))) for _ in range(30000)]
esc_cases += ["**bold**", "[a](b)", "[a](b) and [c](d)", "> quote", ">>> q", "# h", "### h", "#### h", "- l", "  - l", "\n- l", "a\n\n-b", "@everyone", "@here", "@" + "1" * 17, "@" + "1" * 21, "@!" + "1" * 18, "https://a.b/c_d_e.", "<https://a.b/c_d>", "<a:/b>", "x_y_z", "", " ", "-"]
ws = "AbC 日本語 Élan Ωmega ΑΣ Ⅷ ①② ½ ٣ क्षि ก็ ｱｲｳ 😀 _ - . ! ' \" ( ) / ß İ ǅ ǆ ᾳ ﬁ ㍿ 〇 ー ゛゙ ♥ ⓐ Ⓐ ₁ ²".split(" ")
slug_cases = [["".join(random.choice(ws + [" ", "-", "_", "--"]) for _ in range(random.randint(0, 8))), "usr_12345678-1234-1234-1234-123456789abc"] for _ in range(6000)]
slug_cases += [["x" * 200, "usr_aaaaaaaa"], ["あ" * 100, "u"], ["", "usr_abcdef12-0000-0000-0000-000000000000"], ["  ", "abc"], ["---", "abc"]]
W = "wrld_aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
toks = ["~hidden(usr_o)", "~friends(usr_f)", "~private(usr_p)", "~canRequestInvite", "~region(jp)", "~region(eu)", "~group(grp_y)", "~groupAccessType(plus)",
        "~groupAccessType(public)", "~groupAccessType(members)", "~groupAccessType(zz)", "~nonce(abc)", "~hidden()", "~group()", "~friends(usr_x))", "~region", "~hidden(a(b)c)", "~"]
loc_cases = ["", "offline", "private", "traveling", "nocolon", W + ":", W + ":123", ":x", "wrld_x:1:2~region(jp)"]
loc_cases += [W + ":" + str(random.randint(1, 99999)) + "".join(random.sample(toks, random.randint(0, 4))) for _ in range(3000)]
def uid(n): return f"usr_{n:08x}-0000-0000-0000-{n:012x}"
names = [None, "", "Alice", "alice", "ALICE", "Bob", "日本語の名前", "Émile", "x" * 120, "あ" * 90, "A_B*C", "名前 (…123abc)", "Zed", "zed"]
choice_cases = []
for _ in range(600):
    rows = [{"uid": uid(random.randint(0, 60)), "name": random.choice(names)} for _ in range(random.randint(0, 70))]
    choice_cases.append({"rows": rows, "current": random.choice(["", " ", "ali", "ALICE", "日本", "usr_", "0000", "zzz", "123", "é", uid(3)[:12], "(…"])})
def stat():
    h = [random.choice([0, 0, 0, 1, 2, 3, 4, 7, 10]) for _ in range(24)]
    ks = random.sample(["active", "join me", "ask me", "busy", "unknown", "offline", "weird"], random.randint(0, 5))
    st = [[k, float(random.choice([0, 600, 1800, 3600, 5400, 7200, 100000, random.randint(1, 500000)]))] for k in ks]
    return {"logins": sum(h) if random.random() < .8 else random.randint(0, 5), "hours": h, "status": st, "seconds": float(sum(v for _, v in st))}
def merge(per):
    tot = {"logins": 0, "hours": [0] * 24, "status": {}, "seconds": 0.0}
    for p in per.values():
        tot["logins"] += p["logins"]; tot["seconds"] += p["seconds"]
        for i, v in enumerate(p["hours"]): tot["hours"][i] += v
        for k, v in p["status"]: tot["status"][k] = tot["status"].get(k, 0.0) + v
    tot["status"] = [[k, v] for k, v in tot["status"].items()]
    return tot
now = time.time()
stats_cases = []
for lang in ("ja", "en"):
    for i in range(400):
        n = random.choice([0, 1, 1, 3, 8, 10, 11, 15])
        us = [uid(k) for k in random.sample(range(100), n)]
        per = {u: stat() for u in us}
        if i % 7 == 0: per = {u: {"logins": 0, "hours": [0] * 24, "status": [], "seconds": 0.0} for u in us}
        if i % 11 == 0 and us: per[us[0]]["logins"] = 0; per[us[0]]["seconds"] = 5400.0
        end = now - random.randint(0, 5) * 3600 * 7; start = end - random.choice([3600, 86400, 86400 * 7, 86400 * 90, 86400 * 365.5])
        with_names = n > 0 and i % 3 != 0
        stats_cases.append({"lang": lang, "title": random.choice(["📊 Alice のログイン統計", "📊 ログイン統計 (登録フレンド全員)"]), "start": start, "end": end,
                            "per": per, "total": merge(per), "names": [[u, random.choice(names)] for u in us] if with_names else None})
list_cases = []
for lang in ("ja", "en"):
    for _ in range(80):
        rows = [{"uid": uid(k), "name": random.choice(["Alice", "Bob_*", "日本語", "[a](b)", "- x"]), "channel_id": random.randint(10**17, 10**18), "active": random.choice([1, 1, 1, 0])} for k in range(random.choice([0, 1, 9, 10, 11, 25, 40]))]
        ov = {r["uid"]: random.random() < .5 for r in rows if random.random() < .3}
        list_cases.append({"lang": lang, "rows": rows, "show_uid": random.random() < .5, "mention": random.choice([None, True, False]), "overrides": ov})
json.dump({"esc": esc_cases, "slug": slug_cases, "loc": loc_cases, "choices": choice_cases, "stats": stats_cases, "lists": list_cases}, open("/tmp/logic_in.json", "w"))

# ---- Python版の結果 ----
exp = {"esc": [B.esc(s) for s in esc_cases], "slug": [B.slug(*c) for c in slug_cases]}
exp["loc"] = [None if (p := B.parse_location(l)) is None else [p[0], p[1], p[2], p[3], list(p[4]) if p[4] else None] for l in loc_cases]
exp["choices"] = [[[c.name, c.value] for c in B.make_choices(x["rows"], x["current"])] for x in choice_cases]
tz = ZoneInfo("Asia/Tokyo")
def norm(d): d = dict(d); d.pop("type", None); d.pop("flags", None); return d
exp["stats"] = []
for c in stats_cases:
    i18n.set_lang(c["lang"])
    per = {u: {"logins": p["logins"], "hours": p["hours"], "status": dict((k, v) for k, v in p["status"]), "seconds": p["seconds"]} for u, p in c["per"].items()}
    tot = c["total"]; res = {"per": per, "total": {"logins": tot["logins"], "hours": tot["hours"], "status": dict((k, v) for k, v in tot["status"]), "seconds": tot["seconds"]}}
    names = {u: n for u, n in c["names"]} if c["names"] else None
    exp["stats"].append(norm(B.stats_embed(c["title"], res, c["start"], c["end"], tz, names).to_dict()))
exp["lists"] = []
async def lists():
    for c in list_cases:
        i18n.set_lang(c["lang"])
        lv = B.ListView(c["rows"], c["show_uid"], c["mention"], c["overrides"]); views = []
        for p in range(lv.pages):
            lv.page = p; lv._sync()
            views.append({"embed": norm(lv.embed().to_dict()), "prev_disabled": lv.prev.disabled, "next_disabled": lv.next.disabled})
        exp["lists"].append({"pages": lv.pages, "views": views})
asyncio.run(lists())
subprocess.run([str(root / "target/debug/examples/logic_cases")], check=True)
rs = json.load(open("/tmp/logic_rs.json"))
bad = 0
for key in exp:
    mism = [(i, a, b) for i, (a, b) in enumerate(zip(exp[key], rs[key])) if a != b]
    print(f"{key:8s} {len(exp[key]):6d} cases  mismatches: {len(mism)}")
    bad += len(mism)
    for i, a, b in mism[:4]:
        src = {"esc": esc_cases, "slug": slug_cases, "loc": loc_cases}.get(key)
        print("   input:", repr(src[i]) if src else f"#{i}"); print("   python:", json.dumps(a, ensure_ascii=False)[:400]); print("   rust  :", json.dumps(b, ensure_ascii=False)[:400])
print("PARITY OK" if not bad else "PARITY MISMATCH"); sys.exit(1 if bad else 0)
