use std::{
    ops::Deref,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc,
    },
    time::Duration,
};

use dashmap::DashMap;
use poise::serenity_prelude as serenity;
use tokio::sync::RwLock;

use crate::{chat, config};

struct BotDataInner {
    sbuilder: chat::SessionBuilder,
    conf: config::App,
    next_flush: AtomicI64,
    flushing: AtomicBool,
    sessions: RwLock<DashMap<u64, DashMap<u64, chat::Session>>>,
}

#[derive(Clone)]
struct BotData {
    inner: Arc<BotDataInner>,
}

impl BotData {
    fn new(sbuilder: chat::SessionBuilder, conf: config::App) -> Self {
        Self {
            inner: Arc::new(BotDataInner {
                sbuilder,
                conf,
                next_flush: AtomicI64::new(0),
                flushing: AtomicBool::new(false),
                sessions: RwLock::new(DashMap::new()),
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

async fn handle_info_error(err: poise::FrameworkError<'_, BotData, InternalError>) {
    log::error!("unexpected error in info command: {err}");
}

/// Displays information about the model and prompt characteristics
#[poise::command(
    slash_command,
    guild_only,
    guild_cooldown = 4,
    required_permissions = "SEND_MESSAGES",
    on_error = "handle_info_error"
)]
async fn info(ctx: Context<'_>) -> Result<(), InternalError> {
    let data = ctx.data();

    let reset_timestamp = data.next_flush.load(Ordering::Acquire);
    let reset_date = chrono::DateTime::from_timestamp(reset_timestamp, 0)
        .unwrap()
        .format("%v, %R");

    let history_size = data.conf.chat.history_size;
    let model = &data.conf.ai_provider.model;

    let embed = serenity::CreateEmbed::new()
        .title("Characteristics")
        .description(
            "**Note:** prompt messages are removed\n\
            when session limit has been reached",
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
    let message = poise::CreateReply::default().embed(embed).reply(true);
    ctx.send(message).await?;

    Ok(())
}

async fn handle_prompt_error(err: poise::FrameworkError<'_, BotData, InternalError>) {
    log::error!("unexpected error in prompt command: {err}");

    if let poise::FrameworkError::Command { ctx, .. } = err {
        let embed = serenity::CreateEmbed::new()
            .title(":red_circle: Failed to send message, as an unexpected error occurred");
        let message = poise::CreateReply::default().embed(embed).reply(true);
        let _ = ctx.send(message).await;
    }
}

/// Sends a message and waits for the model's response
#[poise::command(
    slash_command,
    guild_only,
    user_cooldown = 2,
    required_permissions = "SEND_MESSAGES",
    on_error = "handle_prompt_error"
)]
async fn prompt(
    ctx: Context<'_>,
    #[description = "message to send"] content: String,
) -> Result<(), InternalError> {
    let data = ctx.data();

    if content.len() > data.conf.chat.prompt_size as usize {
        let embed = serenity::CreateEmbed::new().title(format!(
            ":red_circle: Message must be {} tokens max",
            data.conf.chat.prompt_size
        ));
        let message = poise::CreateReply::default().embed(embed).reply(true);
        ctx.send(message).await?;

        return Ok(());
    }

    if data.flushing.load(Ordering::Acquire) {
        let embed = serenity::CreateEmbed::new()
            .title(":yellow_circle: History is being flushed, wait a little more");
        let message = poise::CreateReply::default().embed(embed).reply(true);
        ctx.send(message).await?;

        return Ok(());
    }

    let guild_id = ctx.guild_id().unwrap().get();
    let author_id = ctx.author().id.get();

    let sessions = data.sessions.read().await;

    let guild_sessions = sessions.entry(guild_id).or_insert_with(DashMap::new);
    let mut session = guild_sessions
        .entry(author_id)
        .or_insert_with(|| data.sbuilder.create_chat());

    let response = session.send_message(content).await?;

    ctx.reply(response).await?;

    Ok(())
}

fn start_sessions_flusher(flush_days: u8, data: BotData) {
    let timeout = Duration::from_secs(flush_days as u64 * 3600);

    tokio::spawn(async move {
        loop {
            let next_flush = (chrono::Local::now() + timeout).timestamp();
            data.next_flush.store(next_flush, Ordering::Release);

            tokio::time::sleep(timeout).await;

            data.flushing.store(true, Ordering::Release);
            data.sessions.write().await.clear();
            data.flushing.store(false, Ordering::Release);
        }
    });
}

fn build_framework(conf: &config::App) -> poise::Framework<BotData, InternalError> {
    let sbuilder = chat::SessionBuilder::new(
        conf.ai_provider.api_key.clone(),
        conf.ai_provider.model.clone(),
        conf.chat.history_size as usize,
    );

    let data = BotData::new(sbuilder, conf.clone());

    start_sessions_flusher(conf.chat.flush_days, data.clone());

    poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![info(), prompt()],
            ..Default::default()
        })
        .setup(|ctx, _, framework| {
            Box::pin(async move {
                let commands = &framework.options().commands;
                let create_commands = poise::builtins::create_application_commands(commands);
                serenity::Command::set_global_commands(ctx, create_commands).await?;

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

    serenity::ClientBuilder::new(bot.discord_token, intents)
        .framework(framework)
        .await
}

pub async fn run(config: config::App) -> Result<(), Error> {
    let framework = build_framework(&config);

    let mut client = build_client(config.bot, framework)
        .await
        .map_err(Error::Creation)?;

    client.start().await.map_err(Error::Initialization)
}
