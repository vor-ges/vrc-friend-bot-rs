# 動かし方 (Rust版 vrc-friend-bot)

Python版と**同じ設定ファイル(.env)・同じデータ(bot.db / .vrc_auth)**で動く。Python版をそのまま置き換えられる。

## 1. 必要なもの

- Rust 1.89 以上 (https://rustup.rs → `rustup update stable`)
- Cコンパイラ (SQLiteを同梱ビルドするため。Ubuntu/Debian: `sudo apt install build-essential` / macOS: `xcode-select --install` / Windows: Visual Studio Build Tools)
- Discord Bot と各種ID(Python版と同じもの)。Botの招待には `bot` と `applications.commands` のスコープ、
  権限は「チャンネルの管理」「メッセージを送信」「埋め込みリンク」が必要 (フレンド用チャンネルを作るため)

## 2. 設定 (.env)

実行ファイルを動かすフォルダに、Python版と同じ `.env` を置く (`upstream/.env.example` が雛形)。

```
DISCORD_TOKEN=...            # Botのトークン
GUILD_ID=...                 # Botを動かすサーバーのID
MAIN_CHANNEL_ID=...          # コマンドを受け付けるメインチャンネルのID
OWNER_ID=...                 # あなたのDiscordユーザーID (メンション先・コマンド実行者)
VRC_USERNAME=...             # VRChatのユーザー名 (メールアドレスでも可)
VRC_PASSWORD=...
VRC_USER_AGENT=VRCFriendBot/1.0 your-email@example.com   # VRChat APIの規約どおり連絡先を入れる
# STATS_TZ=Asia/Tokyo        # 統計の時間帯(省略時は Asia/Tokyo)

# --- ここから下は Python版と並べて動かすときの設定 (詳しくは 6.) ---
# BOT_DB=bot-rs.db           # 登録フレンドとチャンネル・言語/メンション設定の保存先 (Python版の bot.db とは別ファイル。省略時 bot-rs.db)
# STATS_DB=/絶対パス/bot.db   # ログイン記録を持つDB。Python版の bot.db を指すと、記録を共有できる。指定すると既定は読み取り専用
# STATS_MODE=readonly        # readonly = 記録せず読むだけ / record = 記録する。STATS_DB を指定したときの既定は readonly
# FRIEND_CATEGORY=VRC-Friends-rs   # フレンド用チャンネルを作るカテゴリの名前 (無ければ作る。省略時この名前)
# FRIEND_CATEGORY_ID=123456789     # 名前でなく、既存のカテゴリをIDで指定する場合 (これを指定すると名前は無視)
# RUST_LOG=info              # ログの量。既定は「このBotのログだけ info、依存ライブラリは warn 以上」
#                            # 詳しく見たいとき: RUST_LOG=debug  /  RUST_LOG=info,serenity=debug
```

`.env` と `.vrc_auth` は秘密情報。Gitに入れない。

## 3. ビルド

```
cargo build --release
```

できあがる `target/release/vrc-friend-bot` が本体 (単一ファイル、Python環境は不要)。初回は数分かかる。

## 4. 本物のVRChatで先に確認する (おすすめ)

Botを動かす前に、VRChatとの通信だけを確かめられる。パスワードやトークンは表示しない。

```
VRC_USERNAME=... VRC_PASSWORD=... VRC_USER_AGENT="VRCFriendBot/1.0 you@example.com" \
  cargo run --release --example live_check usr_xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
```

`[ OK ] login` → `get_token` → `get_friends_all (N人)` → `get_user` と進めばよい。
`[FAIL]` が出たら、その行をそのまま共有してほしい (`vrc.rs` を直す)。

## 5. 起動

### 初回だけ: 2FAを入力する

**ターミナルから直接**起動する (systemdなどのバックグラウンドではなく)。

```
cd (.env のあるフォルダ)
./target/release/vrc-friend-bot
```

VRChatに2FAが設定されていれば「認証アプリの2FAコード:」(またはメール)と聞かれるので入力する。
成功すると `.vrc_auth` ができて、以後は入力なしで起動する。
ログに `slash commands registered` → `VRChat login OK` → `pipeline connected` → `friend sync done` と出れば動いている。

Python版の `.vrc_auth` が既にあれば、それをそのまま使えるので2FAは聞かれない。

### 常駐させる (Linux / systemd)

`/etc/systemd/system/vrc-friend-bot.service`:

```
[Unit]
Description=VRC Friend Bot (Rust)
After=network-online.target
Wants=network-online.target

[Service]
WorkingDirectory=/opt/vrc-friend-bot
ExecStart=/opt/vrc-friend-bot/vrc-friend-bot
Restart=always
RestartSec=10
# 秘密のファイルは本人だけが読めるようにする: chmod 600 .env .vrc_auth bot.db

[Install]
WantedBy=multi-user.target
```

```
sudo systemctl daemon-reload
sudo systemctl enable --now vrc-friend-bot
journalctl -u vrc-friend-bot -f     # ログを見る
```

`/opt/vrc-friend-bot/` には `vrc-friend-bot`(実行ファイル)・`.env`・`.vrc_auth`・`bot.db` を置く。

> サーバーが ARM (Oracle Cloud の Ampere など) でPCがx86の場合、PCでビルドしたファイルは動かない。
> サーバー上で `cargo build --release` するか (メモリが少なければ `-j1` やスワップを足す)、`cross` で aarch64 向けにビルドする。

## 6. Python版と並べて動かす / 切り替える

**考え方**: データは「共有するもの」と「Botごとに別のもの」に分ける。

| データ | 置き場所 | 理由 |
|---|---|---|
| ログイン記録 (`activity` `vrc_friends` `stats_meta`) | **共有** (長く溜めたPython版の `bot.db` を `STATS_DB` で参照) | VRChatアカウントの事実で、Botごとに違わない |
| 登録フレンドとチャンネルID (`friends`)・言語/メンション設定 (`bot_settings`) | **Botごとに別** (`BOT_DB`) | DiscordのチャンネルはBotごとに違う |

登録は別ファイルなので、**Python版の25人がRust版に勝手に読み込まれて、存在しないチャンネルに通知を送ることはない**。
片方で `/remove` しても、もう片方には影響しない。

### 手順 (Python版は動かしたまま始められる)

1. **(任意・推奨) 共有DBをWALモードにする**。Python版が書き込み中でも、Rust版が同時に読めるようになる。DBファイルに永続する設定で、1回だけ。
   ```
   python3 -c "import sqlite3; print(sqlite3.connect('/絶対パス/bot.db').execute('PRAGMA journal_mode=WAL').fetchone())"
   ```
2. Rust版の `.env` に追加する:
   ```
   BOT_DB=bot-rs.db
   STATS_DB=/絶対パス/bot.db        # Python版の bot.db
   FRIEND_CATEGORY=VRC-Friends-rs
   ```
   `STATS_DB` を指定したので **既定は読み取り専用** (`STATS_MODE=readonly`)。記録はPython版が続け、
   Rust版は読むだけ (通知・`/stats`・`/add` の候補は動く)。**記録が二重にならない** (ログイン回数が2倍になるのを防ぐ)。
   読み取り専用のあいだは、共有DBに1行も書かない (書こうとしてもDB側が拒否する)。
3. 起動して、テスト用に1人 `/add` してみる。`VRC-Friends-rs` カテゴリにチャンネルが作られる。
4. **25人の登録を移す** (元のDBは読むだけで、書き換えない):
   ```
   ./vrc-friend-bot import /絶対パス/bot.db            # まず確認だけ。誰がどうなるか一覧が出る
   ./vrc-friend-bot import /絶対パス/bot.db --apply    # 実行: VRC-Friends-rs に新しいチャンネルを作って登録
   ./vrc-friend-bot check                              # 登録フレンドのチャンネルが実在してBotから見えるか確認
   ```
   - 言語とメンションの設定も、こちらに無いものだけコピーされる。
   - 何度実行しても、登録済みの人は飛ばされる (チャンネルが増えない)。失敗した人だけやり直せる。
   - 存在しないチャンネルは登録しない。`check` は、存在しない/見えないチャンネルを一覧にする。
5. しばらく両方を動かして、通知と `/stats` を見比べる。**同じ人の通知が、Python版のチャンネルとRust版のチャンネルに両方届く**(別チャンネルなので混ざらない)。

### Rust版を「記録担当」に切り替える (Python版をやめる日)

1. Python版を止める。
2. `.env` の `STATS_MODE=record` にして (または行を消さず `STATS_MODE=record` を足して) 起動する。
   以後、Rust版が同じ `bot.db` にそのまま記録を続ける (テーブルの形は同じ)。
3. 念のため: Python版がまだ動いている間に `record` で起動すると、**起動を断る**
   (「別のBotが記録中」。`STATS_FORCE=1` で承知の上なら続けられる)。
4. チャンネルをそのまま引き継ぎたいとき (新しいチャンネルを作りたくないとき) は、Python版を止めたあとで
   `import ... --apply --reuse-channels` を使う。Python版のチャンネルIDをそのまま登録する (実在しなければ登録しない)。
   **Python版を止める前には使わない** (同じチャンネルに2つのBotが書き込むため)。

### 戻したいとき

Rust版を止めて、Python版を起動すればよい (統計テーブルは同じ形。Rust版は `alive` と `rust_alive` を足すだけ)。

### 完全に引っ越す場合 (共有せず、全部コピーする)

`bot.db` をRust版のフォルダにコピーして `BOT_DB=bot.db` にし、`STATS_DB` は指定しない。1つのファイルに登録と記録が入り、
Rust版が記録も担当する。Python版は必ず止めてから。

> `BOT_DB` に置いたDBはRust版だけが使う。`STATS_DB` と `BOT_DB` を**同じファイルにはできない** (登録を共有してしまうため。起動時に断る)。

## 7. 動作確認チェックリスト

Discordのメインチャンネルで:

1. `/list` が出る (登録フレンドが並ぶ。10人を超えるとページ送りの ◀ ▶ が出る)
2. `/stats` で統計の埋め込みが出る / `/stats who:` で候補が出る
3. `/add target:` で候補が出る → 追加するとチャンネルが作られる (カテゴリ `VRC-Friends`)
4. `/remove` で「削除する / キャンセル」ボタンが出る (60秒で期限切れ)
5. `/lang` と `/mention` が動く
6. フレンドがゲームにログインすると、そのフレンドのチャンネルに通知が出る

うまくいかないときは `RUST_LOG=debug` で起動して、ログを見る。

## 8. よくあるつまずき

| 症状 | 原因と対処 |
|---|---|
| `環境変数 X が設定されていません` | `.env` が実行するフォルダにない / 値が空 |
| `Discordに接続できません (DISCORD_TOKEN を確認)` | トークンの間違い・コピー時の余分な空白 |
| `スラッシュコマンドを登録できません` | `GUILD_ID` の間違い、またはBotの招待に `applications.commands` が無い (招待し直す) |
| `VRChatの2FA認証が必要です` | 初回はターミナルから直接起動して、コードを入力する |
| `VRChatにログインできません` | `VRC_USERNAME` / `VRC_PASSWORD`、または連続失敗によるレート制限 (しばらく待つ) |
| チャンネル作成に失敗しました | Botに「チャンネルの管理」権限が無い |
| コマンドが出ない / 古い | Discordを再読み込み (Ctrl+R)。サーバーごとのコマンドなので反映は即時 |
