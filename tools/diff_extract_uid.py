#!/usr/bin/env python3
"""Python版 extract_uid と Rust版を、同じ入力(約4000件)で突き合わせる差分テスト。
使い方: python3 tools/diff_extract_uid.py   (cargo build --example uid_cases が済んでいること)"""
import itertools, json, pathlib, subprocess, sys, re
from urllib.parse import urlparse
root = pathlib.Path(__file__).resolve().parent.parent
# vrc.py は vrchatapi を import するので、extract_uid の定義だけを切り出して使う
src = (root / "upstream" / "vrc.py").read_text(encoding="utf-8")
ns = {"re": re, "urlparse": urlparse}
start = src.index("UID_RE = "); end = src.index("def explain_error")
exec(src[start:end], ns)
py = ns["extract_uid"]

U = "usr_12345678-1234-1234-1234-123456789abc"
UU = U.upper(); UM = "USR_12345678-1234-1234-1234-123456789ABC"
schemes = ["https", "http", "HTTPS", "Http", "ftp", "", "javascript", "h ttps", "https+x", "1http"]
hosts = ["vrchat.com", "www.vrchat.com", "VRCHAT.COM", "Www.VRChat.com", "vrchat.com.", "a@vrchat.com", "vrchat.com:443", "vrchat.com@x.com",
         "vrchat.com\t", " vrchat.com", "vrchat.com\\x", "[vrchat.com]", "evil.com", "vrchat.co", "api.vrchat.com", ""]
paths = [f"/home/user/{U}", f"/HOME/USER/{U}", f"/Home/User/{UU}", f"/home/user/{UM}", f"/home/user/{U}/", f"/home/user/{U}//", f"/home/user/{U};a",
         f"/home/user/{U}/;a", f"/home/user/{U};a/b", f"/home/user/{U}%2F", f"/home/user/ {U}", f"/home//user/{U}", f"/home/user/{U}x", f"/home/user/x{U}",
         f"/home/world/{U}", f"/home/user/", "/home/user", "/", "", f"/home/user/{U[:-1]}", f"/home/user/{U}\n", f"/home/user/{U}\t/", f"//home/user/{U}", f"/./home/user/{U}"]
suffixes = ["", "?", "#", "?x=1#y", ";", "?a=/home/user/" + U, "#/home/user/" + U]
cases = [f"{s}://{h}{p}{x}" for s, h, p, x in itertools.product(schemes, hosts, paths, suffixes)]
cases += [f"{s}:{h}{p}" for s, h, p in itertools.product(schemes[:3], hosts[:3], paths[:3])]   # // なし
cases += [f"{s}:/{h}{p}" for s, h, p in itertools.product(schemes[:2], hosts[:2], paths[:2])]  # / 1つだけ
cases += [U, UU, UM, U + " ", " " + U, f"<{U}>", f"<<{U}>>", f"< {U} >", f" <{U}> ", f"{U}\n", f"\t{U}", U + "x", "x" + U, U[:-1], U.replace("-", "_"),
          "", " ", "<>", "usr_", "x" * 299, "x" * 301, U * 8, "https://vrchat.com/home/user/" + U + "x" * 260,
          f"https://vrchat.com/home/user/{U}\u00a0", f"\u3000https://vrchat.com/home/user/{U}\u3000", f"https://vrchat.com/home/user/{U}\u2028"]
json.dump(cases, open("/tmp/uid_in.json", "w"))
subprocess.run([str(root / "target/debug/examples/uid_cases")], check=True)
rs = json.load(open("/tmp/uid_rs.json"))
bad = []
for c, r in zip(cases, rs):
    try: p = py(c)
    except Exception as e: p = f"EXC:{type(e).__name__}"
    if p != r: bad.append((c, p, r))
print(f"{len(cases)} cases, accepted by python: {sum(1 for c in cases if py(c))}, mismatches: {len(bad)}")
for c, p, r in bad[:25]: print(" ", repr(c), "python=", p, "rust=", r)
sys.exit(1 if bad else 0)
