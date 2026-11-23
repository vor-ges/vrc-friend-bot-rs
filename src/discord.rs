//! Discord接続層 (serenity)。コアの `Reply` をDiscordのメッセージ/ボタンに変換し、スラッシュコマンド・
//! オートコンプリート・ボタンの操作をコアに渡す。Discordの形に依存する部分はここだけ。
//! キャッシュ機能は使わない (必要な情報はその都度HTTPで取る) ので、メモリが増えない。
use crate::bot::{Core, Discord, DiscordError, Reply, View};
use crate::logic::{Embed, ListView};
use crate::tr;
use serenity::all::*;
use serenity::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Python版の "VRC-Friends" とは別のカテゴリにする (同じサーバーで両方動かしても混ざらない)
pub const DEFAULT_CATEGORY: &str = "VRC-Friends-rs";

/// フレンド用チャンネルを作るカテゴリの指定: 名前(無ければ作る) または ID(既存のカテゴリ)
#[derive(Debug, Clone)]
pub enum CategorySpec {
    Name(String),
    Id(u64),
}

// ---------- コア -> Discord (書き込み) ----------

pub struct DiscordApi {
    http: OnceLock<Arc<Http>>,
    guild: GuildId,
    rt: tokio::runtime::Handle,
    category: CategorySpec,
}

fn status_of(e: &serenity::Error) -> Option<u16> {
    match e {
        serenity::Error::Http(h) => match h {
            HttpError::UnsuccessfulRequest(r) => Some(r.status_code.as_u16()),
            _ => None,
        },
        _ => None,
    }
}

fn derr(e: serenity::Error) -> DiscordError {
    DiscordError { status: status_of(&e), message: e.to_string() }
}

impl DiscordApi {
    pub fn new(guild_id: u64, rt: tokio::runtime::Handle) -> Self {
        DiscordApi { http: OnceLock::new(), guild: GuildId::new(guild_id), rt, category: CategorySpec::Name(DEFAULT_CATEGORY.into()) }
    }
    pub fn with_category(mut self, c: CategorySpec) -> Self {
        self.category = c;
        self
    }
    /// クライアントを作ったあとにHTTPクライアントを渡す (コアはクライアントより先に作るため)。
    pub fn set_http(&self, http: Arc<Http>) {
        let _ = self.http.set(http);
    }
    fn http(&self) -> Result<Arc<Http>, DiscordError> {
        self.http.get().cloned().ok_or(DiscordError { status: None, message: "Discord client is not ready".into() })
    }
}

pub fn to_embed(e: &Embed) -> CreateEmbed {
    let mut c = CreateEmbed::new();
    if let Some(t) = &e.title {
        c = c.title(t);
    }
    if let Some(d) = e.description.as_ref().filter(|d| !d.is_empty()) {
        c = c.description(d);
    }
    if let Some(col) = e.color {
        c = c.colour(Colour::new(col));
    }
    for f in &e.fields {
        c = c.field(&f.name, &f.value, f.inline);
    }
    if let Some(u) = &e.thumbnail {
        c = c.thumbnail(u);
    }
    if let Some(t) = &e.footer {
        c = c.footer(CreateEmbedFooter::new(t));
    }
    c
}

// この関数群は blocking スレッド(イベントのワーカー / spawn_blocking)から呼ばれる想定。
impl Discord for DiscordApi {
    fn send(&self, channel_id: i64, content: Option<&str>, embed: &Embed) -> Result<(), DiscordError> {
        let http = self.http()?;
        let mut m = CreateMessage::new().embed(to_embed(embed));
        if let Some(c) = content {
            m = m.content(c);
        }
        self.rt.block_on(ChannelId::new(channel_id as u64).send_message(&http, m)).map(|_| ()).map_err(derr)
    }

    fn friend_channel(&self, existing: Option<i64>, chan_name: &str, uid: &str) -> Result<i64, DiscordError> {
        let http = self.http()?;
        if let Some(id) = existing {
            match self.rt.block_on(http.get_channel(ChannelId::new(id as u64))) {
                Ok(_) => return Ok(id), // 実在すれば再利用
                Err(e) if status_of(&e) == Some(404) => {}
                Err(e) => return Err(derr(e)),
            }
        }
        let cat = match &self.category {
            CategorySpec::Id(id) => match self.rt.block_on(http.get_channel(ChannelId::new(*id))).map_err(derr)? {
                Channel::Guild(g) if g.kind == ChannelType::Category => g.id,
                _ => return Err(DiscordError { status: None, message: format!("FRIEND_CATEGORY_ID={id} はカテゴリではありません") }),
            },
            CategorySpec::Name(name) => {
                let chans = self.rt.block_on(self.guild.channels(&http)).map_err(derr)?;
                match chans.values().find(|c| c.kind == ChannelType::Category && &c.name == name) {
                    Some(c) => c.id,
                    None => self.rt.block_on(self.guild.create_channel(&http, CreateChannel::new(name).kind(ChannelType::Category))).map_err(derr)?.id,
                }
            }
        };
        let ch = self.rt.block_on(self.guild.create_channel(&http, CreateChannel::new(chan_name).kind(ChannelType::Text).category(cat).topic(uid))).map_err(derr)?;
        Ok(ch.id.get() as i64)
    }

    fn channel_exists(&self, channel_id: i64) -> Result<bool, DiscordError> {
        let http = self.http()?;
        match self.rt.block_on(http.get_channel(ChannelId::new(channel_id as u64))) {
            Ok(_) => Ok(true),
            Err(e) if matches!(status_of(&e), Some(404) | Some(403)) => Ok(false), // 無い / 見えない
            Err(e) => Err(derr(e)),
        }
    }
}

// ---------- スラッシュコマンドの定義 ----------

fn opt(kind: CommandOptionType, name: &str, desc: &str) -> CreateCommandOption {
    CreateCommandOption::new(kind, name, desc)
}

/// /add /remove /list /stats /lang /mention (グループやサブコマンドは使わない、トップレベルの1語)
pub fn command_defs() -> Vec<CreateCommand> {
    vec![
        CreateCommand::new("add")
            .description("フレンドを選んで(またはURLで)通知先に追加")
            .add_option(opt(CommandOptionType::String, "target", "フレンドを選択、またはプロフィールURL (https://vrchat.com/home/user/usr_...)").required(true).set_autocomplete(true)),
        CreateCommand::new("remove")
            .description("登録済みフレンドを削除 (チャンネルは残ります)")
            .add_option(opt(CommandOptionType::String, "target", "候補から選択 (表示名の末尾はUIDの下6桁)").required(true).set_autocomplete(true)),
        CreateCommand::new("list")
            .description("登録一覧")
            .add_option(opt(CommandOptionType::Boolean, "show_uid", "UIDも表示する").required(false))
            .add_option(opt(CommandOptionType::Boolean, "include_removed", "削除済みも表示する").required(false)),
        CreateCommand::new("stats")
            .description("フレンドのログイン時間帯・ステータス使用割合の統計")
            .add_option(opt(CommandOptionType::Integer, "days", "直近何日分を集計するか (省略時は収集開始から全期間)").required(false).min_int_value(1).max_int_value(365))
            .add_option(opt(CommandOptionType::String, "who", "フレンドを選択 (省略時は登録中の全員。削除済み・未登録の人も選べます)").required(false).set_autocomplete(true)),
        CreateCommand::new("lang")
            .description("表示言語の切り替え / Switch language: /lang en, /lang ja")
            .add_option(opt(CommandOptionType::String, "code", "言語コード (ISO 639) / Language code: en, ja").required(true).min_length(2).max_length(16)),
        CreateCommand::new("mention")
            .description("オンライン通知のメンション切り替え / Toggle online mention: /mention on|off|reset")
            .add_option(
                opt(CommandOptionType::String, "state", "on / off / reset (reset: フレンド用チャンネルで個別設定を解除)")
                    .required(true)
                    .add_string_choice("on", "on")
                    .add_string_choice("off", "off")
                    .add_string_choice("reset", "reset"),
            ),
    ]
}

// ---------- ボタン付きメッセージ (確認 / 一覧のページ送り) ----------

fn view_rows(v: &View, disabled: bool) -> Vec<CreateActionRow> {
    match v {
        View::Confirm { .. } => vec![CreateActionRow::Buttons(vec![
            CreateButton::new("rm:yes").label(tr!("削除する")).style(ButtonStyle::Danger).disabled(disabled),
            CreateButton::new("rm:no").label(tr!("キャンセル")).style(ButtonStyle::Secondary).disabled(disabled),
        ])],
        View::List(lv) => vec![CreateActionRow::Buttons(vec![
            CreateButton::new("ls:prev").label("◀").style(ButtonStyle::Secondary).disabled(disabled || lv.prev_disabled()),
            CreateButton::new("ls:next").label("▶").style(ButtonStyle::Secondary).disabled(disabled || lv.next_disabled()),
        ])],
    }
}

struct Entry {
    view: View,
    channel: ChannelId,
    message: MessageId,
    deadline: Instant,
    timeout: Duration,
}

type Views = Arc<Mutex<HashMap<u64, Entry>>>;

fn lockv<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn message_of(r: &Reply, rows: Vec<CreateActionRow>) -> CreateInteractionResponseMessage {
    let mut m = CreateInteractionResponseMessage::new().ephemeral(r.ephemeral);
    if let Some(c) = &r.content {
        m = m.content(c);
    }
    if let Some(e) = &r.embed {
        m = m.embed(to_embed(e));
    }
    if !rows.is_empty() {
        m = m.components(rows);
    }
    m
}

/// ボタンからの更新: embed が無ければ既存の埋め込みを消し、ボタンも外す。
fn update_of(r: &Reply, rows: Vec<CreateActionRow>) -> CreateInteractionResponseMessage {
    let mut m = CreateInteractionResponseMessage::new().embeds(r.embed.iter().map(to_embed).collect()).components(rows);
    if let Some(c) = &r.content {
        m = m.content(c);
    }
    m
}

// ---------- イベントの受け口 ----------

pub struct Handler {
    pub core: Arc<Core>,
    views: Views,
}

impl Handler {
    pub fn new(core: Arc<Core>) -> Self {
        Handler { core, views: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// 同期のコアを blocking スレッドで実行する。
    async fn blocking<T: Send + 'static>(&self, f: impl FnOnce(&Core) -> T + Send + 'static) -> Option<T> {
        let core = self.core.clone();
        match tokio::task::spawn_blocking(move || f(&core)).await {
            Ok(v) => Some(v),
            Err(e) => {
                log::error!("command task failed: {e}");
                None
            }
        }
    }

    fn failure() -> Reply {
        Reply { content: Some(tr!("VRChat APIでエラーが発生しました。ログを確認してください。")), ephemeral: true, ..Default::default() }
    }

    async fn respond(&self, ctx: &Context, cmd: &CommandInteraction, r: Reply) {
        let rows = r.view.as_ref().map(|v| view_rows(v, false)).unwrap_or_default();
        if let Err(e) = cmd.create_response(&ctx.http, CreateInteractionResponse::Message(message_of(&r, rows))).await {
            log::error!("respond failed: {e}");
            return;
        }
        if let Some(view) = r.view {
            self.register_view(ctx, cmd, view).await;
        }
    }

    async fn register_view(&self, ctx: &Context, cmd: &CommandInteraction, view: View) {
        let msg = match cmd.get_response(&ctx.http).await {
            Ok(m) => m,
            Err(e) => {
                log::error!("could not fetch the response message: {e}");
                return;
            }
        };
        let timeout = if matches!(view, View::Confirm { .. }) { Duration::from_secs(60) } else { Duration::from_secs(120) };
        let mid = msg.id.get();
        lockv(&self.views).insert(mid, Entry { view, channel: msg.channel_id, message: msg.id, deadline: Instant::now() + timeout, timeout });
        let (views, http, core) = (self.views.clone(), ctx.http.clone(), self.core.clone());
        tokio::spawn(async move {
            loop {
                let dl = match lockv(&views).get(&mid) {
                    Some(e) => e.deadline,
                    None => return, // 操作が済んで片付いた
                };
                tokio::time::sleep_until(tokio::time::Instant::from_std(dl)).await;
                let expired = {
                    let mut g = lockv(&views);
                    match g.get(&mid) {
                        None => return,
                        Some(e) if e.deadline > Instant::now() => None, // 操作で延長された
                        Some(_) => g.remove(&mid),
                    }
                };
                if let Some(e) = expired {
                    let edit = match &e.view {
                        View::Confirm { .. } => EditMessage::new().content(core.confirm_timeout().content.unwrap_or_default()).embeds(vec![]).components(vec![]),
                        View::List(_) => EditMessage::new().components(view_rows(&e.view, true)),
                    };
                    if let Err(err) = e.channel.edit_message(&http, e.message, edit).await {
                        log::warn!("timeout edit failed: {err}"); // 消えたメッセージなどは無視
                    }
                    return;
                }
            }
        });
    }

    async fn on_command(&self, ctx: &Context, cmd: &CommandInteraction) {
        let (user, ch) = (cmd.user.id.get() as i64, cmd.channel_id.get() as i64);
        let o = cmd.data.options();
        let string = |n: &str| o.iter().find(|x| x.name == n).and_then(|x| if let ResolvedValue::String(v) = x.value { Some(v.to_string()) } else { None });
        let int = |n: &str| o.iter().find(|x| x.name == n).and_then(|x| if let ResolvedValue::Integer(v) = x.value { Some(v) } else { None });
        let flag = |n: &str| o.iter().find(|x| x.name == n).and_then(|x| if let ResolvedValue::Boolean(v) = x.value { Some(v) } else { None }).unwrap_or(false);
        let r = match cmd.data.name.as_str() {
            "stats" => {
                let (days, who) = (int("days"), string("who"));
                self.blocking(move |c| c.cmd_stats(user, ch, days, who.as_deref())).await
            }
            "lang" => {
                let code = string("code").unwrap_or_default();
                self.blocking(move |c| c.cmd_lang(user, &code)).await
            }
            "mention" => {
                let state = string("state").unwrap_or_default();
                self.blocking(move |c| c.cmd_mention(user, ch, &state)).await
            }
            "list" => {
                let (show_uid, removed) = (flag("show_uid"), flag("include_removed"));
                self.blocking(move |c| c.cmd_list(user, ch, show_uid, removed)).await
            }
            "remove" => {
                let t = string("target").unwrap_or_default();
                self.blocking(move |c| c.cmd_remove(user, ch, &t)).await
            }
            "add" => {
                let t = string("target").unwrap_or_default();
                match self.blocking(move |c| c.cmd_add_prepare(user, ch, &t)).await {
                    Some(Err(reply)) => Some(reply),
                    Some(Ok(uid)) => {
                        // VRChat APIとチャンネル作成で3秒を超えうるので、先に「考え中」を返す
                        if let Err(e) = cmd.defer(&ctx.http).await {
                            log::error!("defer failed: {e}");
                            return;
                        }
                        let reply = self.blocking(move |c| c.cmd_add_run(user, &uid)).await.unwrap_or_else(Self::failure);
                        let mut f = CreateInteractionResponseFollowup::new();
                        if let Some(c) = &reply.content {
                            f = f.content(c);
                        }
                        if let Some(e) = &reply.embed {
                            f = f.embed(to_embed(e));
                        }
                        if let Err(e) = cmd.create_followup(&ctx.http, f).await {
                            log::error!("followup failed: {e}");
                        }
                        return;
                    }
                    None => None,
                }
            }
            other => {
                log::warn!("unknown command: {other}");
                return;
            }
        };
        self.respond(ctx, cmd, r.unwrap_or_else(Self::failure)).await;
    }

    async fn on_autocomplete(&self, ctx: &Context, ac: &CommandInteraction) {
        let Some(focus) = ac.data.autocomplete() else { return };
        let (cmd, name, current) = (ac.data.name.clone(), focus.name.to_string(), focus.value.to_string());
        let user = ac.user.id.get() as i64;
        let choices = self
            .blocking(move |c| match (cmd.as_str(), name.as_str()) {
                ("add", "target") => c.ac_add(user, &current),
                ("remove", "target") => c.ac_remove(user, &current),
                ("stats", "who") => c.ac_who(user, &current),
                _ => vec![],
            })
            .await
            .unwrap_or_default();
        let mut resp = CreateAutocompleteResponse::new();
        for ch in choices {
            resp = resp.add_string_choice(ch.name, ch.value);
        }
        if let Err(e) = ac.create_response(&ctx.http, CreateInteractionResponse::Autocomplete(resp)).await {
            log::warn!("autocomplete failed: {e}");
        }
    }

    async fn on_component(&self, ctx: &Context, c: &ComponentInteraction) {
        let user = c.user.id.get() as i64;
        if let Some(r) = self.core.view_check(user) {
            let _ = c.create_response(&ctx.http, CreateInteractionResponse::Message(message_of(&r, vec![]))).await;
            return;
        }
        let mid = c.message.id.get();
        let snapshot = lockv(&self.views).get(&mid).map(|e| e.view.clone());
        let Some(view) = snapshot else {
            let _ = c.create_response(&ctx.http, CreateInteractionResponse::Acknowledge).await; // 再起動などで状態が無い
            return;
        };
        let (reply, next_view): (Reply, Option<View>) = match (c.data.custom_id.as_str(), view) {
            ("rm:yes", View::Confirm { uid, .. }) => (self.blocking(move |c| c.confirm_yes(user, &uid)).await.unwrap_or_else(Self::failure), None),
            ("rm:no", View::Confirm { name, .. }) => (self.blocking(move |c| c.confirm_no(&name)).await.unwrap_or_else(Self::failure), None),
            (id @ ("ls:prev" | "ls:next"), View::List(mut lv)) => {
                lv.page = if id == "ls:prev" { lv.page.saturating_sub(1) } else { (lv.page + 1).min(lv.pages - 1) };
                (Reply { embed: Some(lv.embed()), ..Default::default() }, Some(View::List(lv)))
            }
            _ => {
                let _ = c.create_response(&ctx.http, CreateInteractionResponse::Acknowledge).await;
                return;
            }
        };
        let rows = match &next_view {
            Some(v) => {
                let mut g = lockv(&self.views);
                if let Some(e) = g.get_mut(&mid) {
                    e.view = v.clone();
                    e.deadline = Instant::now() + e.timeout; // 操作のたびに期限を延ばす
                }
                view_rows(v, false)
            }
            None => {
                lockv(&self.views).remove(&mid); // 確認は1回きり
                vec![]
            }
        };
        if let Err(e) = c.create_response(&ctx.http, CreateInteractionResponse::UpdateMessage(update_of(&reply, rows))).await {
            log::error!("update failed: {e}");
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        log::info!("logged in as {}", ready.user.name);
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match &interaction {
            Interaction::Command(c) => self.on_command(&ctx, c).await,
            Interaction::Autocomplete(a) => self.on_autocomplete(&ctx, a).await,
            Interaction::Component(c) => self.on_component(&ctx, c).await,
            _ => {}
        }
    }
}

/// 一覧の確認用 (テストで View を作るため)
pub fn list_view_rows(lv: &ListView) -> usize {
    view_rows(&View::List(lv.clone()), false).len()
}
