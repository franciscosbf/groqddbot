use std::{
    ops::Deref,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc,
    },
    time::Duration,
};

use dashmap::DashMap;
use poise::{serenity_prelude as serenity, ReplyHandle};
use tokio::sync::{Mutex, RwLock};

use crate::{chat, config};

const ONE_DAY_IN_SECS: Duration = Duration::from_secs(3600);
const DELETE_MSG_AFTER_SECS: Duration = Duration::from_secs(10);

type GuildId = u64;
type UserId = u64;

type GuildSessions = Arc<DashMap<UserId, ChatSession>>;

#[derive(Clone, Debug)]
struct ChatSession {
    session: Arc<Mutex<chat::Session>>,
}

impl ChatSession {
    fn new(session: chat::Session) -> Self {
        Self {
            session: Arc::new(Mutex::new(session)),
        }
    }

    async fn send_message(&self, content: String) -> Result<String, genai::Error> {
        self.session.lock().await.send_message(content).await
    }

    async fn remove_last_interaction(&self) {
        self.session.lock().await.pop_last_interaction();
    }
}

struct BotDataInner {
    next_flush: AtomicI64,
    flush_timeout: Duration,
    flushing: AtomicBool,
    sbuilder: chat::SessionBuilder,
    sessions: RwLock<DashMap<GuildId, GuildSessions>>,
    conf: config::App,
}

impl BotDataInner {
    async fn session(&self, guild: GuildId, user: UserId) -> ChatSession {
        let sessions = self.sessions.read().await;

        let guild_sessions = {
            sessions
                .entry(guild)
                .or_insert_with(|| Arc::new(DashMap::new()))
                .clone()
        };

        let session = {
            guild_sessions
                .entry(user)
                .or_insert_with(|| ChatSession::new(self.sbuilder.create_chat()))
                .clone()
        };

        session
    }

    fn schedule_next_flush(&self) {
        let next_flush = (chrono::Local::now() + self.flush_timeout).timestamp();
        self.next_flush.store(next_flush, Ordering::Release);
    }

    fn next_flush(&self) -> chrono::DateTime<chrono::Utc> {
        let timestamp = self.next_flush.load(Ordering::Acquire);
        chrono::DateTime::from_timestamp(timestamp, 0).unwrap()
    }

    fn is_flushing(&self) -> bool {
        self.flushing.load(Ordering::Acquire)
    }

    fn flushing(&self, yes: bool) {
        self.flushing.store(yes, Ordering::Release);
    }

    async fn flush(&self) {
        self.flushing(true);
        self.sessions.write().await.clear();
        self.flushing(false);
    }
}

#[derive(Clone)]
struct BotData {
    inner: Arc<BotDataInner>,
}

impl BotData {
    fn new(sbuilder: chat::SessionBuilder, conf: config::App) -> Self {
        Self {
            inner: Arc::new(BotDataInner {
                flush_timeout: ONE_DAY_IN_SECS * conf.chat.flush_days as u32,
                next_flush: AtomicI64::new(0),
                flushing: AtomicBool::new(false),
                sbuilder,
                sessions: RwLock::new(DashMap::new()),
                conf,
            }),
        }
    }
}

impl Deref for BotData {
    type Target = BotDataInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

type InternalError = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, BotData, InternalError>;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to create bot")]
    Creation(#[source] serenity::Error),
    #[error("failed to initialize bot")]
    Initialization(#[source] serenity::Error),
}

async fn send_embedded_reply(
    ctx: Context<'_>,
    embed: serenity::CreateEmbed,
) -> Result<ReplyHandle<'_>, serenity::Error> {
    let message = poise::CreateReply::default().embed(embed).reply(true);
    ctx.send(message).await
}

async fn send_temporary_embedded_reply(
    ctx: Context<'_>,
    embed: serenity::CreateEmbed,
) -> Result<(), serenity::Error> {
    let http = ctx.serenity_context().http.clone();
    let message = send_embedded_reply(ctx, embed)
        .await?
        .into_message()
        .await?;

    tokio::spawn(async move {
        tokio::time::sleep(DELETE_MSG_AFTER_SECS).await;

        let _ = message.delete(http).await;
    });

    Ok(())
}

async fn send_cooldown_alert(ctx: Context<'_>) {
    let embed = serenity::CreateEmbed::new().title(":hotsprings: Hold on, I'm not that fast!");
    if let Err(err) = send_temporary_embedded_reply(ctx, embed).await {
        log::warn!("failed to send cooldown alert: {err}");
    }
}

async fn send_alert_on_info_error(ctx: Context<'_>) {
    let embed =
        serenity::CreateEmbed::new().title(":man_shrugging: Something went wrong and Idk why...");
    if let Err(err) = send_temporary_embedded_reply(ctx, embed).await {
        log::warn!("failed to send alert on error in 'info' command: {err}",);
    }
}

async fn handle_info_error(err: poise::FrameworkError<'_, BotData, InternalError>) {
    match err {
        poise::FrameworkError::Command { ctx, ref error, .. } => {
            log::error!("unexpected error while executing 'info' command: {error}");

            send_alert_on_info_error(ctx).await;
        }
        poise::FrameworkError::CommandPanic { ctx, payload, .. } => {
            log::error!(
                "info command was abruptly stopped (i.e., panicked): {}",
                payload.as_deref().unwrap_or("unknown reason")
            );

            send_alert_on_info_error(ctx).await;
        }
        poise::FrameworkError::CooldownHit { ctx, .. } => {
            send_cooldown_alert(ctx).await;
        }
        poise::FrameworkError::MissingBotPermissions { .. } => (),
        err => log::error!("scary error on 'info' command: {err}"),
    }
}

/// Displays information about the model and prompt characteristics
#[poise::command(
    slash_command,
    guild_only,
    user_cooldown = 2,
    required_permissions = "SEND_MESSAGES",
    on_error = "handle_info_error"
)]
async fn info(ctx: Context<'_>) -> Result<(), InternalError> {
    let data = ctx.data();
    let reset_date = data.next_flush().format("%v, %R");
    let conf = &data.conf;
    let history_size = conf.chat.history_size;
    let model = &conf.ai_provider.model;

    let embed = serenity::CreateEmbed::new()
        .title("Characteristics")
        .description(
            "**Note:** older interactions are removed
            when session limit is reached",
        )
        .field(
            ":wastebasket: | Sessions Reset Date:",
            format!("{}", reset_date),
            false,
        )
        .field(
            ":notepad_spiral: | Session History Size:",
            format!(
                "{} interaction{} per user",
                history_size,
                if history_size > 1 { "s" } else { "" }
            ),
            false,
        )
        .field(":brain: | LLM's Name:", model, false)
        .field(
            ":pencil: | Prompt Message Size Limit:",
            format!("{} tokens (aka characters)", data.conf.chat.prompt_size),
            false,
        );
    send_embedded_reply(ctx, embed).await?;

    Ok(())
}

async fn handle_prompt_error(err: poise::FrameworkError<'_, BotData, InternalError>) {
    match err {
        poise::FrameworkError::Command { ctx, ref error, .. } => {
            log::error!("unexpected error while executing 'prompt' command: {error}");

            let embed = serenity::CreateEmbed::new()
                .title(":skull: Failed to send message. Something went realy bad...");
            let _ = send_embedded_reply(ctx, embed).await;
        }
        poise::FrameworkError::CooldownHit { ctx, .. } => {
            send_cooldown_alert(ctx).await;
        }
        poise::FrameworkError::MissingBotPermissions { .. } => (),
        err => log::error!("scary error on 'prompt' command: {err}"),
    }
}

/// Sends a message and waits for the model's response
#[poise::command(
    slash_command,
    guild_only,
    user_cooldown = 4,
    required_permissions = "SEND_MESSAGES",
    on_error = "handle_prompt_error"
)]
async fn prompt(
    ctx: Context<'_>,
    #[description = "message to send"] content: String,
) -> Result<(), InternalError> {
    let data = ctx.data();
    let conf = &data.conf;

    if content.len() > conf.chat.prompt_size as usize {
        let embed = serenity::CreateEmbed::new().title(format!(
            ":red_circle: Message must be {} tokens max",
            conf.chat.prompt_size
        ));
        send_embedded_reply(ctx, embed).await?;

        return Ok(());
    }

    if data.is_flushing() {
        let embed = serenity::CreateEmbed::new()
            .title(":yellow_circle: History is being flushed, wait a little more");
        send_embedded_reply(ctx, embed).await?;

        return Ok(());
    }

    let guild = ctx.guild_id().unwrap().get();
    let user = ctx.author().id.get();

    let session = data.session(guild, user).await;
    let response = session.send_message(content).await?;

    match ctx.reply(response).await {
        Ok(_) => Ok(()),
        Err(err) => {
            session.remove_last_interaction().await;

            Err(Box::from(err))
        }
    }
}

fn start_sessions_flusher(data: BotData) {
    tokio::spawn(async move {
        loop {
            data.schedule_next_flush();

            tokio::time::sleep(data.flush_timeout).await;

            data.flush().await;
        }
    });
}

async fn event_handler(
    _ctx: &serenity::Context,
    event: &serenity::FullEvent,
    _framework: poise::FrameworkContext<'_, BotData, InternalError>,
    _data: &BotData,
) -> Result<(), InternalError> {
    match event {
        serenity::FullEvent::Ready { data_about_bot } => {
            let servers = data_about_bot.guilds.len();
            let session = data_about_bot.session_id.as_str();
            log::info!(
                "bot has been connected to discord on {} server{} (session '{}')",
                servers,
                if servers != 1 { "s" } else { "" },
                session
            );
        }
        serenity::FullEvent::Resume { .. } => {
            log::info!("bot was reconnected to discord");
        }
        serenity::FullEvent::ShardsReady { total_shards } => {
            let shards = total_shards;
            log::info!("bot shards are ready (loaded {})", shards);
        }
        _ => (),
    }

    Ok(())
}

fn build_framework(conf: &config::App) -> poise::Framework<BotData, InternalError> {
    let sbuilder = chat::SessionBuilder::new(
        conf.ai_provider.api_key.clone(),
        conf.ai_provider.model.clone(),
        conf.chat.history_size as usize,
    );

    let data = BotData::new(sbuilder, conf.clone());

    poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![info(), prompt()],
            event_handler: |ctx, event, framework, data| {
                Box::pin(event_handler(ctx, event, framework, data))
            },
            ..Default::default()
        })
        .setup(|ctx, _ready, framework| {
            Box::pin(async move {
                let commands = &framework.options().commands;
                let create_commands = poise::builtins::create_application_commands(commands);
                serenity::Command::set_global_commands(ctx, create_commands).await?;

                start_sessions_flusher(data.clone());

                Ok(data)
            })
        })
        .build()
}

async fn build_client(
    bot: config::Bot,
    framework: poise::Framework<BotData, InternalError>,
) -> Result<serenity::Client, serenity::Error> {
    let intents = serenity::GatewayIntents::GUILD_MESSAGES;
    let activity = serenity::ActivityData {
        name: "Stealing LLM's access for my own benefit".to_string(),
        kind: serenity::ActivityType::Playing,
        state: None,
        url: None,
    };
    let status = serenity::OnlineStatus::Online;

    serenity::ClientBuilder::new(bot.discord_token, intents)
        .framework(framework)
        .activity(activity)
        .status(status)
        .await
}

pub async fn run(config: config::App) -> Result<(), Error> {
    let framework = build_framework(&config);

    let mut client = build_client(config.bot, framework)
        .await
        .map_err(Error::Creation)?;

    client.start().await.map_err(Error::Initialization)
}
