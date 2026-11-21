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

## 6. Python版からの切り替え

1. Python版を止める (`sudo systemctl stop ...`)。**両方を同時に動かさない** (通知が二重になり、同じ `bot.db` に同時に書き込むため)
2. `bot.db` `.env` `.vrc_auth` をバックアップする
3. それらをRust版のフォルダにコピーして起動する (テーブルは同じ形なので変換は不要)
4. 戻したいときは、Rust版を止めてPython版を起動すればよい (Rust版が足すテーブルはPython版と同じ)

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
