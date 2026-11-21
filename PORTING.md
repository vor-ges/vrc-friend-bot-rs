# PORTING.md — Python版 → Rust版 反映状況

元リポジトリ: https://github.com/vor-ges/vrc-friend-bot

## 現在の基準 (Rust版が追従しているPython版)

- 状態: **GitHubにコミット済み・基準確定** (2026-11-21 照合)
- コードの基準コミット: **`8addfa9`** "Track activity for all VRChat friends everytime"
  (bot.py / db.py / i18n.py / stats.py / vrc.py / test_local.py / requirements.txt / .env.example が `upstream/` と完全一致)
- 照合時点のmain先頭: `f417004` (8addfa9 と 7c6a016 のマージ。コード差分なし)
- 差異: `README.md` の末尾改行のみ (内容は同じ)
- Python版のバージョン表記: `1.2.1`

次回の差分取得は `git diff 8addfa9 origin/main` を起点にする。
スナップショット本体は `upstream/` にある (Rust版の反映後も消さないこと)。

## ファイルのSHA-256 (upstream/)

```
7aec2e5e22a52cd6ed3c157e7854d62a0b023fd9e058a90164ebd8d75783a828  bot.py
c8e4de1ad68d0facdd67b0e53869ad17060713fae2b5b2caa59247adf912f9d7  db.py
b0624eced99ed1b4b33b9f1c2194bcbfb0d565b92980076b3e046b5f6c1ee1f2  i18n.py
398f13ae90dea6d11697b4db3774a6d4785ca6d78dbe59a6c2ba12468b75e49f  stats.py
3842fe077fd2689c91a555c5bb92f12cc7c0be97010d70f74016c65313ee75d3  vrc.py
45b16f683b882814b62b02cb23e4882b7c39538a4241e4dc6d362cce4f75d210  test_local.py
8f8ac5156f5b4e16efaf0d5c607c2fc8919839d48e07b3c52a614d8b24404b7f  requirements.txt
78c88a49a0d187bf022d59970055d28bb72c7e1239762e90077a4b72b5f7ae5e  .env.example
f8045096e47179df08d681ef657b9f84a04d24bf37ff2398db21ab64c4d8c271  README.md
d148bebbe22dee80c3ca9c00a8e47df91adb5908701328b06156d6d03acc7913  README.ja.md
```

## 更新手順 (差分だけを反映する)

1. 最新のPython版を取得する (GitHubのclone、または最新のZip)
2. `upstream/` と比較し、変更されたファイル・行だけを抽出する
3. その変更に対応するRust側の関数・構造体だけを修正する (全面的な書き直しはしない)
4. 反映が済んだら `upstream/` を新しい版に置き換え、このファイルの
   「現在の基準」とSHA-256を更新する

## Python → Rust 対応表

| Python | Rust | 状態 |
|---|---|---|
| `db.py` | `src/db.rs` | ✅ 完了 |
| `stats.py` | `src/stats.rs` | ✅ 完了 (Pythonと出力一致を確認済み) |
| `i18n.py` | `src/i18n.rs` + `src/i18n_table.rs`(自動生成) | ✅ 完了 |
| `vrc.py` | `src/vrc.rs` | ✅ 完了 (モックサーバーで検証済み / **実機確認はこれから**) |
| `bot.py` の純粋ロジック | `src/logic.rs` | ✅ 完了 (Python版の関数と約4万件で一致を確認済み) |
| `bot.py` の `Bot` / `FriendCommands` | `src/bot.rs` | ✅ 完了 (偽のDiscord/VRChatでテスト済み) |
| `bot.py` のDiscord操作・ボタン・コマンド登録 | `src/discord.rs` | ✅ 完了 (**実機確認はこれから**) |
| `bot.py` の `ws_loop` | `src/ws.rs` | ✅ 完了 (手元のWebSocketサーバーでテスト済み / 実機確認はこれから) |
| `bot.py` の起動 (`Bot().run`) | `src/main.rs` | ✅ 完了 (**実機確認はこれから**) |
| `test_local.py` | `tests/*.rs` | 全35件。Discord.py固有のテスト(コマンドツリー等)は `tests/discord_defs.rs` で定義を直接検証 |

DBスキーマ (`friends` / `activity` / `stats_meta` / `vrc_friends` / `bot_settings`) は
既存の `bot.db` と互換を保つ (同じ `bot.db` をPython版とRust版が読める)。

## Python版との意図的な違い

- `Stats.summarize` の `status` は、Pythonのdictと同じ**挿入順を保つ** `StatusMap`。
  (`bot.py` は同率のとき挿入順で並べるため、`HashMap` にすると表示が揺れる)
- `stats.make_tz` の読み込み失敗時は Asia/Tokyo (= UTC+9)。tzdata は chrono-tz に同梱され、OSに無くても動く。
- `tr("...", name=x)` は `tr!("...", name = x)` マクロ。書式は `{name}` `{x:.1f}` `{h:02d}` `{{ }}` に対応。
  対応外の書式は Python同様に例外にせず、翻訳 → 原文 → 未整形の原文 の順で諦める。
- `summarize` は uid順にストリーム処理し、全行をメモリに載せない。
- コアは**同期**で書き、Discord/VRChatは `trait Discord` / `trait VrcApi` の窓口越し (テストで偽物に差し替える)。
  WebSocketのイベントは専用スレッドで1件ずつ順番に処理し (Python版の `await handle()` と同じ)、
  `sync_friends` は別スレッドで走る。状態の更新は1つのロックの中で行い、通信はロックの外。
- Discordは serenity を**キャッシュ無し**・`GUILDS` intent のみで使う。フレンド用チャンネルの存在確認はその都度HTTP(404なら作り直す)。
- ボタン(確認 / 一覧のページ送り)の状態はメモリ上 (Python版と同じく、再起動するとボタンは効かなくなる)。
- 人数表示で `n_users` が取れなかったとき、Python版は "None/16" と出すところを "?/16" にした。
- `esc()` は discord.py 2.7 の `escape_markdown`(見出し `#` と箇条書き `-` のエスケープを含む)に合わせてある。

- `vrc.py` は vrchatapi SDK をやめ、同期クライアント `ureq`(rustls) + `serde_json::Value` で必要な8本のAPIだけを直接呼ぶ。
  Botからは `tokio::task::spawn_blocking` で呼ぶ想定 (Python版の `asyncio.to_thread` と同じ)。
- 例外の代わりに `VrcError` (`Status(u16)` / `Timeout` / `NeedsReauth` / `Transport` / `Parse`)。
  `NeedsReauth` は Python版の `EOFError` (2FAの入力が必要だが標準入力が無い) に当たる。
- クッキー(`auth` / `twoFactorAuth`)は自前で保持する。`.vrc_auth` は Python版と同じ形式で、そのまま引き継げる (Unixでは 0600 で書く)。
- `extract_uid` は Python の `urlparse` と同じ切り分けを自前で実装 (26,941件の差分テストで Python と不一致0件)。

## 実機で確認が必要な点 (このサンドボックスでは確かめられない)

`RUNNING.md` の手順で確認する。まず `live_check` (VRChatだけ)、次に実際のBot (Discord)。

**VRChat (`vrc.rs`)**
1. Basic認証は「ユーザー名とパスワードをURLエンコードしてからbase64」にしている (VRChat公式の仕様に合わせた)。記号を含むパスワードでだけ差が出る。
2. 2FAの判定は `GET /auth/user` の `requiresTwoFactorAuth` の値 (`emailOtp` / `totp` / `otp`) による。
3. 認証クッキーの名前は `auth` / `twoFactorAuth`。
4. インスタンスIDの `(` `)` は `%28` `%29` に変換して送る。
5. インスタンスの人数・定員は `n_users` / `capacity` キーから読む。
6. pipeline(WebSocket)は `wss://pipeline.vrchat.cloud/?authToken=...` + User-Agent で接続し、メッセージは `{"type","content"}` (contentはJSON文字列のことも)。

**Discord (`discord.rs`)**
7. スラッシュコマンドの登録 (ギルド単位の一括上書き) と、`defer` → フォローアップ (`/add`)。
8. オートコンプリートの応答 (`add` / `remove` / `stats who`)、ボタン (`rm:yes` `rm:no` `ls:prev` `ls:next`) による更新、期限切れでの無効化。
9. チャンネル作成 (カテゴリ `VRC-Friends`、topic=UID) と、権限が無いときのエラー表示。

## ツール

- `python3 tools/gen_i18n_table.py` : `upstream/i18n.py` から `src/i18n_table.rs` を再生成 (Python側の訳が変わったら実行)
- `python3 tools/gen_py_tr_keys.py` : Python側の `tr()` キー一覧を `tests/data/py_tr_keys.json` に再生成 (翻訳キー網羅テスト用)
- `python3 tools/diff_extract_uid.py` : `extract_uid` を Python版と同じ入力で突き合わせる差分テスト (`cargo build --example uid_cases` が先)
- `python3 tools/parity_bench.py gen` → `cargo run --release --example bench` → `python3 tools/parity_bench.py cmp`
  : 同じSQLiteファイルをPython版とRust版で集計し、結果の一致と速度を比べる
- `python3 tools/diff_logic.py` : `esc` / `slug` / `parse_location` / `make_choices` / `stats_embed` / `ListView` を、Python版(本物の bot.py の関数)とRust版で約4万件突き合わせる (`cargo build --example logic_cases` が先)。要 `pip install discord.py`
- `python3 tools/gen_py_tr_keys.py` を実行すると、Python側の `tr()` キー一覧が `tests/data/py_tr_keys.json` に再生成される

## ビルド環境のメモ

- Rust 1.89 以上 (これでビルド・テストした)。`Cargo.lock` はコミットすること。
- `rusqlite` は `bundled` (SQLite同梱)、TLSは rustls (OpenSSL不要)。初回のreleaseビルドは数分。
- ロジックだけ試すときは `cargo test --no-default-features` (Discord/WebSocketの重い依存を含めず速い)。

## 次にやること

1. `RUNNING.md` の手順で実機確認 (`live_check` → Bot) し、うまくいかない箇所を調整
2. Python版の更新を取り込むとき: `git diff <基準コミット> origin/main` の差分だけを、対応するRustの関数に反映する
   (`tools/gen_i18n_table.py` と `tools/gen_py_tr_keys.py` を再実行 → `tests/i18n_keys.rs` が文言の過不足を教える)
