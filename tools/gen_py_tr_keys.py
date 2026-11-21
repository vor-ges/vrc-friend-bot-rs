#!/usr/bin/env python3
"""upstream/bot.py と upstream/vrc.py の tr("...") の第1引数(日本語キー)を ast で集めて tests/data/py_tr_keys.json に出す。
Rust側の翻訳キー網羅性テストの基準になる。Python版を更新したら再実行する。"""
import ast, json, pathlib
root = pathlib.Path(__file__).resolve().parent.parent
keys = set()
for f in ("bot.py", "vrc.py"):
    for n in ast.walk(ast.parse((root / "upstream" / f).read_text(encoding="utf-8"))):
        if isinstance(n, ast.Call) and getattr(n.func, "id", "") == "tr" and n.args and isinstance(n.args[0], ast.Constant):
            keys.add(n.args[0].value)
(root / "tests" / "data" / "py_tr_keys.json").write_text(json.dumps(sorted(keys), ensure_ascii=False, indent=1), encoding="utf-8")
print(len(keys), "keys")
